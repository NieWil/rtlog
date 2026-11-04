#!/usr/bin/env python3
"""Flash and RAM cost of rtlog on a Cortex-M4F, as a markdown table.

Builds size/mcu in four variants (no logging, rtlog with 1 and 10 call sites,
and the same 10 messages with core::fmt) and reads section sizes with
llvm-size (from the llvm-tools-preview rustup component).

usage: scripts/size_report.py [--out report.md]
"""
import argparse
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MCU = ROOT / "size" / "mcu"
TARGET = "thumbv7em-none-eabihf"
FLASH = (".vector_table", ".text", ".rodata", ".data")
DESCRIPTORS = ("linkme_SITES", "linkm2_SITES")
RAM = (".data", ".bss", ".uninit")


def llvm_size():
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    host = next(l.split(": ")[1] for l in subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
                if l.startswith("host: "))
    return pathlib.Path(sysroot) / "lib" / "rustlib" / host / "bin" / "llvm-size"


def build(variant):
    subprocess.check_call(["cargo", "build", "-q", "--release", "--features", variant], cwd=MCU)
    elf = MCU / "target" / TARGET / "release" / "rtlog-size-mcu"
    out = subprocess.check_output([str(llvm_size()), "-A", str(elf)], text=True)
    sizes = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].isdigit():
            sizes[parts[0]] = int(parts[1])
    return {
        "code": sizes.get(".text", 0),
        "rodata": sizes.get(".rodata", 0),
        "desc": sum(sizes.get(s, 0) for s in DESCRIPTORS),
        "flash": sum(sizes.get(s, 0) for s in FLASH + DESCRIPTORS),
        "ram": sum(sizes.get(s, 0) for s in RAM),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    a = ap.parse_args()
    v = {name: build(name) for name in ("baseline", "rtlog1", "rtlog10", "fmt10")}
    base = v["baseline"]
    rows = [
        ("No logging (baseline)", "baseline"),
        ("rtlog, 1 call site", "rtlog1"),
        ("rtlog, 10 call sites", "rtlog10"),
        ("`core::fmt`, same 10 messages", "fmt10"),
    ]
    per_site_code = (v["rtlog10"]["code"] - v["rtlog1"]["code"]) / 9
    per_site_desc = (v["rtlog10"]["desc"] - v["rtlog1"]["desc"]) / 9
    fixed = v["rtlog1"]["code"] - base["code"] - per_site_code
    lines = [
        "## rtlog size on Cortex-M4F",
        "",
        "`size/mcu` firmware, `thumbv7em-none-eabihf`, `opt-level = \"s\"`, fat LTO. Bytes; flash includes the "
        "vector table. Descriptors are the contract entries (site ID, level, format string pointer); "
        "they are read on the host, never on the log path.",
        "",
        "| Variant | Code (`.text`) | Read-only data | Site descriptors | Flash total | RAM |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for label, key in rows:
        s = v[key]
        lines.append("| %s | %d | %d | %d | %d | %d |" % (label, s["code"], s["rodata"], s["desc"], s["flash"], s["ram"]))
    fmt_extra = v["fmt10"]["flash"] - base["flash"]
    rt_extra = v["rtlog10"]["flash"] - base["flash"]
    lines += [
        "",
        "| Derived | Value |",
        "| --- | ---: |",
        "| rtlog runtime, fixed code (ring, drain, encoder) | %.0f B |" % fixed,
        "| Code per additional call site | %.0f B |" % per_site_code,
        "| Descriptor per call site (strippable in a later release) | %.0f B |" % per_site_desc,
        "| RAM: one 1 KiB ring plus state | %d B |" % v["rtlog1"]["ram"],
        "| Flash for 10 messages: rtlog vs `core::fmt` | %d B vs %d B (%.1f× smaller) |"
        % (rt_extra, fmt_extra, fmt_extra / max(rt_extra, 1)),
    ]
    report = "\n".join(lines) + "\n"
    print(report)
    if a.out:
        pathlib.Path(a.out).write_text(report)


if __name__ == "__main__":
    sys.exit(main())
