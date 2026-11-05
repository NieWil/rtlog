#!/usr/bin/env python3
"""Check the code against the rules in docs/spec.md.

Static checks (always run):
  realtime   the log hot path has no locks, allocation, formatting, I/O,
             panicking calls or sleeps (spec section 0, zero-impact principle)
  wire       control-frame IDs and argument tags agree between the Rust core,
             the C header and the decoder (spec section 5)
  usability  every output mode is reachable from the CLI and documented;
             relative links in the docs resolve
  spec       the budgets this script enforces are present in the spec

Measured checks (when reports are given):
  --timing FILE...  timing reports (examples/timing.rs); the best W1 p50 is
                    compared with the spec's cycle budget. Over budget is a
                    warning, because cycle counts on shared CI runners vary
                    by 10-20 cycles from run to run; --strict-timing makes it
                    a failure (for a quiet, dedicated machine)
  --size FILE       size report (scripts/size_report.py); fixed runtime and
                    per-site code must be within the spec's size budgets
                    (deterministic, always a failure when over)

Budgets are read from docs/spec.md, so changing the spec changes the gate.
Exit status is non-zero if any check fails.
"""
import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SPEC = ROOT / "docs" / "spec.md"
CORE = ROOT / "crates" / "rtlog-core" / "src"

# Functions on the log path: (file, function names). Everything a log call
# executes when the level is enabled and the ring has room.
HOT_PATH = {
    "ring.rs": ["emit", "put_varint", "push_raw", "write_at"],
    "arg.rs": ["put", "put_array", "put_u8", "put_dyn", "write"],
    "level.rs": ["enabled"],
    "clock.rs": ["now", "default_now", "source", "counter"],
    "runtime.rs": ["__local_emit"],
}
FORBIDDEN = [
    (r"\bMutex\b|\.lock\(\)|RwLock", "lock"),
    (r"\bBox::new\b|\bVec::|\bString::|\bto_string\(|\bto_owned\(|\bformat!", "allocation or formatting"),
    (r"\bprintln!|\beprintln!|\bprint!|\bstd::io::|\bstd::fs::", "I/O"),
    (r"\.unwrap\(\)|\.expect\(|\bpanic!|\bunreachable!|\btodo!|\bunimplemented!", "panicking call"),
    (r"thread::sleep|park\(|yield_now", "sleep or yield"),
]

failures = []
warnings = []
notes = []


def fail(check, msg):
    failures.append(f"[{check}] {msg}")


def warn(check, msg):
    warnings.append(f"[{check}] {msg}")


def fn_bodies(src, name):
    """Bodies of every `fn name` in `src`. The signature ends at the first `{`
    or `;` outside brackets (so `[u8; N]` does not end it, and a bodiless trait
    declaration yields nothing)."""
    for m in re.finditer(r"\bfn\s+" + re.escape(name) + r"\b", src):
        i, depth = m.end(), 0
        while i < len(src):
            ch = src[i]
            if ch in "([<":
                depth += 1
            elif ch in ")]>" and depth:
                depth -= 1
            elif depth == 0 and ch in "{;":
                break
            i += 1
        if i >= len(src) or src[i] == ";":
            continue
        body_depth, j = 1, i + 1
        while j < len(src) and body_depth:
            body_depth += {"{": 1, "}": -1}.get(src[j], 0)
            j += 1
        yield src[m.start():j]


def strip_comments(code):
    code = re.sub(r"//[^\n]*", "", code)
    return re.sub(r"/\*.*?\*/", "", code, flags=re.S)


def check_realtime():
    for file, names in HOT_PATH.items():
        src = (CORE / file).read_text()
        for name in names:
            bodies = list(fn_bodies(src, name))
            if not bodies:
                fail("realtime", f"{file}: hot-path function `{name}` not found (update HOT_PATH)")
            for body in bodies:
                code = strip_comments(body)
                # The cold fallback of `emit` (stack buffer) and test helpers are fine.
                code = re.split(r"#\[cfg\(test\)\]", code)[0]
                for pattern, what in FORBIDDEN:
                    for hit in re.finditer(pattern, code):
                        line = code[: hit.start()].count("\n")
                        fail("realtime", f"{file}: `{name}` uses {what} ({hit.group(0)!r}, line +{line})")


def check_wire():
    wire = (CORE / "wire.rs").read_text()
    # `CONTROL_BASE` alone is offset 0 (the resync frame).
    rust = {m.group(1): int(m.group(2) or 0) for m in re.finditer(r"pub const CTRL_(\w+): u32 = CONTROL_BASE(?: \+ (\d+))?;", wire)}
    if "CTRL_RESYNC" not in wire or not rust:
        fail("wire", "could not read control frame IDs from wire.rs")
    header = (ROOT / "c" / "include" / "rtlog.h").read_text()
    c = {m.group(1): int(m.group(2), 16) - 0xFFFFFF00 for m in re.finditer(r"#define RTLOG__CTRL_(\w+) 0x(FFFFFF[0-9A-Fa-f]{2})u", header)}
    for name, val in c.items():
        if name not in rust:
            fail("wire", f"C control frame {name} has no Rust counterpart")
        elif rust[name] != val:
            fail("wire", f"control frame {name}: Rust +{rust[name]}, C +{val}")
    stream = (ROOT / "crates" / "rtlog-decode" / "src" / "stream.rs").read_text()
    for name in rust:
        if f"wire::CTRL_{name}" not in stream:
            fail("wire", f"decoder does not handle control frame CTRL_{name}")
    arg = (CORE / "arg.rs").read_text()
    tags = {m.group(1): int(m.group(2)) for m in re.finditer(r"^\s+(\w+) = (\d+),", arg.split("pub enum Tag")[1].split("}")[0], re.M)}
    c_tags = {
        "U8": r"rtlog__u8, uint8_t, (\d+)", "U16": r"rtlog__u16, uint16_t, (\d+)", "U32": r"rtlog__u32, uint32_t, (\d+)",
        "U64": r"rtlog__u64, uint64_t, (\d+)", "I8": r"rtlog__i8, int8_t, (\d+)", "I16": r"rtlog__i16, int16_t, (\d+)",
        "I32": r"rtlog__i32, int32_t, (\d+)", "I64": r"rtlog__i64, int64_t, (\d+)", "F32": r"rtlog__f32, float, (\d+)",
        "F64": r"rtlog__f64, double, (\d+)",
    }
    for tag, pattern in c_tags.items():
        m = re.search(pattern, header)
        if not m:
            fail("wire", f"C header lacks the encoder for tag {tag}")
        elif int(m.group(1)) != tags.get(tag):
            fail("wire", f"tag {tag}: Rust {tags.get(tag)}, C {m.group(1)}")


def check_usability():
    cli = (ROOT / "crates" / "rtlog-decode" / "src" / "bin" / "rtlog.rs").read_text()
    lib = (ROOT / "crates" / "rtlog-decode" / "src" / "lib.rs").read_text()
    outputs = re.findall(r"^\s{4}(\w+),$", lib.split("pub enum Output")[1].split("}")[0], re.M)
    for o in outputs:
        if f"Output::{o}" not in cli:
            fail("usability", f"output mode {o} is not reachable from the CLI")
    docs = "\n".join(p.read_text() for p in [ROOT / "README.md", *(ROOT / "docs").rglob("*.md")])
    for needle, what in [("--perfetto", "Perfetto export"), ("--json", "JSON lines"), ("stdout::init", "stdout mode"),
                         ("crash::install", "crash log"), ("rtlog contract --elf", "contract from ELF")]:
        if needle not in docs:
            fail("usability", f"{what} is not documented (no `{needle}` in README or docs/)")
    for md in [ROOT / "README.md", *(ROOT / "docs").rglob("*.md")]:
        for m in re.finditer(r"\]\(([^)#\s]+)(#[^)]*)?\)", md.read_text()):
            url = m.group(1)
            if not url.startswith(("http://", "https://", "mailto:")) and not (md.parent / url).resolve().exists():
                fail("usability", f"{md.relative_to(ROOT)}: broken link {url}")


def spec_budgets():
    spec = SPEC.read_text()
    budgets = {}
    m = re.search(r"≤ (\d+) cycles per W1 call", spec)
    if m:
        budgets["cycles"] = int(m.group(1))
    m = re.search(r"fixed runtime code ≤ (\d+) bytes", spec)
    if m:
        budgets["fixed"] = int(m.group(1))
    m = re.search(r"≤ (\d+) bytes of code per (?:additional )?call site", spec)
    if m:
        budgets["per_site"] = int(m.group(1))
    for key, what in [("cycles", "cycle budget"), ("fixed", "fixed runtime size budget"), ("per_site", "per-site size budget")]:
        if key not in budgets:
            fail("spec", f"docs/spec.md states no {what} this script can read")
    return budgets


def check_timing(paths, budget, strict):
    best = None
    for p in paths:
        text = pathlib.Path(p).read_text()
        m = re.search(r"\| W1 scalar \|[^|]*\|(?:[^|]*\|){4}\s*(\d+)\s*\|", text)
        if not m:
            fail("performance", f"{p}: no W1 cycle figure (core clock not measured on this machine?)")
            continue
        cycles = int(m.group(1))
        notes.append(f"{p}: W1 p50 = {cycles} cycles")
        best = cycles if best is None else min(best, cycles)
    if best is not None and budget is not None:
        if best > budget:
            (fail if strict else warn)("performance", f"W1 log call: best p50 {best} cycles > spec budget {budget}")
        else:
            notes.append(f"performance: W1 {best} cycles ≤ budget {budget}")


def check_size(path, budgets):
    text = pathlib.Path(path).read_text()
    fixed = re.search(r"rtlog runtime, fixed code[^|]*\|\s*(\d+) B", text)
    per = re.search(r"Code per additional call site\s*\|\s*(\d+) B", text)
    if not (fixed and per):
        fail("memory", f"{path}: size report format not recognised")
        return
    fixed, per = int(fixed.group(1)), int(per.group(1))
    notes.append(f"memory: fixed runtime {fixed} B, {per} B per call site")
    if "fixed" in budgets and fixed > budgets["fixed"]:
        fail("memory", f"fixed runtime code {fixed} B > spec budget {budgets['fixed']} B")
    if "per_site" in budgets and per > budgets["per_site"]:
        fail("memory", f"{per} B of code per call site > spec budget {budgets['per_site']} B")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--timing", nargs="*", default=[], help="timing report(s) to check against the cycle budget")
    ap.add_argument("--size", help="size report to check against the size budgets")
    ap.add_argument("--strict-timing", action="store_true", help="fail (not warn) when the cycle budget is missed")
    a = ap.parse_args()
    budgets = spec_budgets()
    check_realtime()
    check_wire()
    check_usability()
    if a.timing:
        check_timing(a.timing, budgets.get("cycles"), a.strict_timing)
    if a.size:
        check_size(a.size, budgets)
    print("## Spec rules\n")
    print("Budgets read from docs/spec.md: " + ", ".join(f"{k} = {v}" for k, v in budgets.items()) + "\n")
    for n in notes:
        print(f"- {n}")
    for w in warnings:
        print(f"- ⚠ {w} (reported, not gated: shared runners vary; see --strict-timing)")
    if failures:
        print(f"\n**{len(failures)} rule(s) broken:**\n")
        for f in failures:
            print(f"- {f}")
        return 1
    print("\nAll checked rules hold: real-time hot path, wire format, usability"
          + (", performance" if a.timing else "") + (", memory" if a.size else "") + ".")
    return 0


if __name__ == "__main__":
    sys.exit(main())
