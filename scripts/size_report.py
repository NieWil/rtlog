#!/usr/bin/env python3
"""Flash and RAM cost of rtlog on a Cortex-M4F, as a markdown table.

Builds size/mcu in five variants (no logging, rtlog with 1 and 10 call sites,
the 10 call sites with rtlog's `strip` feature, and the same 10 messages with
core::fmt) and reads section sizes with
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
STRINGS = ("rtlog_strs",)
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
    sizes, addrs = {}, {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[1].isdigit():
            sizes[parts[0]] = int(parts[1])
            addrs[parts[0]] = int(parts[2])
    # Descriptor sections count as flash where the linker script loads them
    # (address in FLASH); with the `strip` feature they are in the ELF only.
    loaded = [s for s in DESCRIPTORS + STRINGS if addrs.get(s, 0) >= 0x08000000]
    return {
        "code": sizes.get(".text", 0),
        "rodata": sizes.get(".rodata", 0),
        "desc": sum(sizes.get(s, 0) for s in DESCRIPTORS + STRINGS),
        "desc_flash": sum(sizes.get(s, 0) for s in loaded),
        "flash": sum(sizes.get(s, 0) for s in FLASH) + sum(sizes.get(s, 0) for s in loaded),
        "ram": sum(sizes.get(s, 0) for s in RAM),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    a = ap.parse_args()
    v = {name: build(name) for name in ("baseline", "rtlog1", "rtlog10", "rtlog10strip", "fmt10")}
    base = v["baseline"]
    rows = [
        ("No logging (baseline)", "baseline"),
        ("rtlog, 1 call site", "rtlog1"),
        ("rtlog, 10 call sites", "rtlog10"),
        ("rtlog, 10 call sites, `strip` feature", "rtlog10strip"),
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
        "they are read on the host, never on the log path. With the `strip` feature the descriptors and "
        "their strings are in the ELF file but not in flash.",
        "",
        "| Variant | Code (`.text`) | Read-only data | Site descriptors in flash | Flash total | RAM |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for label, key in rows:
        s = v[key]
        lines.append("| %s | %d | %d | %d | %d | %d |" % (label, s["code"], s["rodata"], s["desc_flash"], s["flash"], s["ram"]))
    fmt_extra = v["fmt10"]["flash"] - base["flash"]
    rt_extra = v["rtlog10"]["flash"] - base["flash"]
    strip_extra = v["rtlog10strip"]["flash"] - base["flash"]
    if v["rtlog10strip"]["desc_flash"] != 0 or v["rtlog10strip"]["desc"] == 0:
        sys.exit("strip variant: descriptors must be in the ELF (%d B) and not in flash (%d B)"
                 % (v["rtlog10strip"]["desc"], v["rtlog10strip"]["desc_flash"]))
    lines += [
        "",
        "| Derived | Value |",
        "| --- | ---: |",
        "| rtlog runtime, fixed code (ring, drain, encoder) | %.0f B |" % fixed,
        "| Code per additional call site | %.0f B |" % per_site_code,
        "| Descriptor per call site (0 B in flash with the `strip` feature) | %.0f B |" % per_site_desc,
        "| RAM: one 1 KiB ring plus state | %d B |" % v["rtlog1"]["ram"],
        "| Flash for 10 messages: rtlog vs `core::fmt` | %d B vs %d B (%.1f× smaller) |"
        % (rt_extra, fmt_extra, fmt_extra / max(rt_extra, 1)),
        "| Flash for 10 messages with the `strip` feature | %d B (%.1f× smaller than `core::fmt`, %d B less than unstripped) |"
        % (strip_extra, fmt_extra / max(strip_extra, 1), rt_extra - strip_extra),
    ]
    report = "\n".join(lines) + "\n"
    print(report)
    if a.out:
        pathlib.Path(a.out).write_text(report)


if __name__ == "__main__":
    sys.exit(main())
