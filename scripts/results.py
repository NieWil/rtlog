#!/usr/bin/env python3
"""Check the board results in results/ and regenerate results/README.md.

Each result is the timing report of one board for one rtlog version:
results/<version>/<board>.md, where <board> is a lowercase slug (a-z, 0-9,
-). Two kinds:

- Host report: the unedited output of the `timing` example (or the demo's
  timing.md), Rust on an OS. Platform: Board, CPU, OS, rtlog, Rust, Target.
- MCU report: written by `results.py mcu` from the Zephyr benchmark
  firmware's output (c/zephyr/samples/benchmark), C on a microcontroller,
  timed with the core cycle counter. Platform: Board, CPU, Core clock, OS,
  rtlog, API, Toolchain, Target.

The rtlog version in the report must match the folder.

  python3 scripts/results.py           # check, then rewrite results/README.md
  python3 scripts/results.py --check   # check, and fail if README.md is stale (CI)
  python3 scripts/results.py mcu capture.txt --board "..." ... --out results/<v>/<board>.md
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "results"
INDEX = RESULTS / "README.md"
HOST_TITLE = "## rtlog timing report"
MCU_TITLE = "## rtlog MCU timing report"
REQUIRED = {
    HOST_TITLE: ["Board", "CPU", "OS", "rtlog", "Rust", "Target"],
    MCU_TITLE: ["Board", "CPU", "Core clock", "OS", "rtlog", "API", "Toolchain", "Target"],
}
PLATFORM_KEYS = {k for keys in REQUIRED.values() for k in keys} | {"Measured core clock", "Timestamp counter"}
# Benchmark firmware case names, in report order, with their table labels.
MCU_CASES = [
    ("baseline", "Baseline (counter read, no call)"),
    ("runtime_disabled", "Disabled (level below runtime filter)"),
    ("no_args", "No arguments"),
    ("one_u32", "One u32"),
    ("three_u32", "Three u32"),
    ("string_7_bytes", "7-byte string"),
    ("span_pair", "Span enter + exit"),
    ("full_ring_one_u32", "One u32, ring full (dropped)"),
]
SLUG = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?$")


def rows(text):
    """Every `| a | b | ... |` table row as a list of stripped cells."""
    out = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("|") and line.endswith("|") and not set(line) <= set("|-: "):
            out.append([c.strip() for c in line.strip("|").split("|")])
    return out


def parse(path):
    """(platform dict, workload rows by name, metrics dict) of one report."""
    text = path.read_text(encoding="utf-8")
    platform, workloads, metrics = {}, {}, {}
    for cells in rows(text):
        if len(cells) == 2:
            key, value = cells
            if key in PLATFORM_KEYS:
                platform.setdefault(key, value.strip("`"))
            else:
                metrics[key] = value
        elif len(cells) == 7 and cells[0] not in ("Workload",):
            workloads.setdefault(cells[0], cells)
    return text, platform, workloads, metrics


def check(path):
    """Problems with one results file, as strings (empty when it is fine)."""
    rel = path.relative_to(ROOT)
    problems = []
    version_dir = path.parent.name
    if not VERSION.match(version_dir):
        problems.append(f"{rel}: folder '{version_dir}' is not a version like 0.2.4")
    if not SLUG.match(path.stem):
        problems.append(f"{rel}: file name must be a lowercase slug like raspberry-pi-5.md")
    text, platform, workloads, _ = parse(path)
    title = kind(text)
    if title is None:
        problems.append(f"{rel}: not a timing report (it must start with '{HOST_TITLE}' or '{MCU_TITLE}')")
        return problems
    for key in REQUIRED[title]:
        if not platform.get(key):
            problems.append(f"{rel}: Platform table has no '{key}' row (regenerate it with rtlog {version_dir} or later)")
    if platform.get("rtlog") and platform["rtlog"] != version_dir:
        problems.append(f"{rel}: report says rtlog {platform['rtlog']}, but it is in results/{version_dir}/")
    if platform.get("Board", "").startswith("unknown"):
        problems.append(f"{rel}: board is unknown; rerun with --board NAME")
    needed = "W1 scalar" if title == HOST_TITLE else "One u32"
    if needed not in workloads:
        problems.append(f"{rel}: no '{needed}' row in the workload table")
    return problems


def kind(text):
    """The report's title line, or None if it is neither kind."""
    first = text.split("\n", 1)[0].strip()
    return first if first in REQUIRED else None


def version_key(v):
    nums = re.match(r"(\d+)\.(\d+)\.(\d+)", v)
    return tuple(int(n) for n in nums.groups()) if nums else (0, 0, 0)


def index(files):
    """The generated results/README.md."""
    out = [
        "# Results from users' boards",
        "",
        "Timing reports from real hardware, one file per board and rtlog version. To add yours, see [CONTRIBUTING.md](../CONTRIBUTING.md#share-your-boards-results).",
        "",
        "- **Host:** the `timing` example (or the demo's `timing.md`), Rust on an OS. W1 is `info!(\"x={} y={}\", u32, u32)`; times are the median (p50) of per-batch means.",
        "- **Microcontrollers:** the Zephyr benchmark firmware (`c/zephyr/samples/benchmark`), the C header timed with the core cycle counter; raw median cycles per call, wrapper included.",
        "",
        "A call's cost in cycles compares across machines of one kind; its time depends on the clock. Generated by `scripts/results.py`; do not edit by hand.",
        "",
    ]
    if not files:
        out += ["No results yet. Be the first: run the demo on your board and open a pull request or an issue.", ""]
        return "\n".join(out)
    by_version = {}
    for f in files:
        by_version.setdefault(f.parent.name, []).append(f)
    for version in sorted(by_version, key=version_key, reverse=True):
        out += [f"## rtlog {version}", ""]
        parsed = [(f, *parse(f)) for f in sorted(by_version[version])]
        host = [x for x in parsed if kind(x[1]) == HOST_TITLE]
        mcu = [x for x in parsed if kind(x[1]) == MCU_TITLE]
        if host:
            out += [
                "**Host (Rust, `timing` example)**",
                "",
                "| Board | CPU | OS | Target | W1 p50 ns | W1 p50 cycles | Disabled p50 ns | Throughput, 1 thread |",
                "| --- | --- | --- | --- | ---: | ---: | ---: | ---: |",
            ]
            for f, _, p, w, m in host:
                w1 = w.get("W1 scalar", [""] * 7)
                off = w.get("Disabled", [""] * 7)
                board = f"[{p['Board']}]({version}/{f.name})"
                out.append(
                    f"| {board} | {p['CPU']} | {p['OS']} | `{p['Target']}` | {w1[3]} | {w1[6]} | {off[3]} | {m.get('Ring + drain throughput, 1 thread', '')} |"
                )
            out.append("")
        if mcu:
            out += [
                "**Microcontrollers (C header, core cycle counter; median cycles per call)**",
                "",
                "| Board | CPU | Clock | OS | No arguments | One u32 | Three u32 | 7-byte string | Disabled |",
                "| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |",
            ]
            for f, _, p, w, _m in mcu:
                med = lambda case: w.get(case, [""] * 7)[2]
                board = f"[{p['Board']}]({version}/{f.name})"
                out.append(
                    f"| {board} | {p['CPU']} | {p['Core clock']} | {p['OS']} | {med('No arguments')} | {med('One u32')} | {med('Three u32')} | {med('7-byte string')} | {med('Disabled (level below runtime filter)')} |"
                )
            out.append("")
        if mcu and host:
            out += ["Host and microcontroller numbers measure different code paths (Rust macros vs the C header) with different clocks; compare within a table, not across.", ""]
    return "\n".join(out)


def read_mcu(paths):
    """Per-case lists of (min, median, p95, max, drops) and the firmware's
    settings, from serial captures or CSV files of the benchmark firmware.

    Accepts the firmware's own output (a `RTLOG_BENCH core_hz=...` line, then
    `case,min_cycles,...` rows) and the combined CSV in docs/measurements
    (`logger,run,case,...`, rtlog rows only)."""
    cases, settings = {}, {}
    for path in paths:
        for line in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
            line = line.strip()
            if line.startswith("RTLOG_BENCH "):
                settings.update(kv.split("=", 1) for kv in line.split()[1:] if "=" in kv)
                continue
            cells = [c.strip() for c in line.split(",")]
            if len(cells) == 8 and cells[0] in ("rtlog", "zephyr"):
                if cells[0] != "rtlog":
                    continue
                cells = cells[2:]
            if len(cells) != 6 or not all(c.isdigit() for c in cells[1:]):
                continue
            cases.setdefault(cells[0], []).append(tuple(int(c) for c in cells[1:]))
    return cases, settings


def mcu_report(args):
    """Write an MCU timing report from benchmark firmware output."""
    cases, settings = read_mcu(args.inputs)
    missing = [c for c, _ in MCU_CASES if c not in cases]
    if missing:
        sys.exit(f"no rows for case(s) {', '.join(missing)} in {', '.join(args.inputs)}")
    clock_mhz = args.clock_mhz or (int(settings["core_hz"]) / 1e6 if "core_hz" in settings else None)
    if not clock_mhz:
        sys.exit("core clock unknown: pass --clock-mhz (the capture has no RTLOG_BENCH core_hz line)")
    runs = len(cases["one_u32"])
    samples = settings.get("samples", args.samples)
    lines = [
        MCU_TITLE,
        "",
        "| Platform | |",
        "| --- | --- |",
        f"| Board | {args.board} |",
        f"| CPU | {args.cpu} |",
        f"| Core clock | {clock_mhz:g} MHz |",
        f"| OS | {args.os} |",
        f"| rtlog | {args.rtlog} |",
        f"| API | {args.api} |",
        f"| Toolchain | {args.toolchain} |",
        f"| Target | `{args.target}` |",
        "",
        f"Benchmark firmware [`c/zephyr/samples/benchmark`](https://github.com/NieWil/rtlog/tree/v{args.rtlog}/c/zephyr/samples/benchmark): "
        f"core cycle counter (DWT CYCCNT), {samples} calls per case and run, {runs} reset run(s), interrupts masked around each call, "
        "ring drained outside the timed interval. Cycles are raw: they include the timing wrapper and counter read (the Baseline row); nothing is subtracted. "
        "Per case: lowest min, median of the runs' medians, highest p95 and max.",
        "",
        "| Case | min cycles | median cycles | p95 cycles | max cycles | median µs | drops |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for case, label in MCU_CASES:
        rows_ = cases[case]
        lo = min(r[0] for r in rows_)
        meds = sorted(r[1] for r in rows_)
        med = meds[len(meds) // 2]
        p95 = max(r[2] for r in rows_)
        hi = max(r[3] for r in rows_)
        drops = sum(r[4] for r in rows_)
        lines.append(f"| {label} | {lo} | {med} | {p95} | {hi} | {med / clock_mhz:.3f} | {drops} |")
    if args.notes:
        lines += ["", args.notes]
    lines.append("")
    text = "\n".join(lines)
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(text, encoding="utf-8")
        print(f"wrote {args.out}")
    else:
        sys.stdout.write(text)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "mcu":
        mp = argparse.ArgumentParser(prog="results.py mcu", description=mcu_report.__doc__)
        mp.add_argument("inputs", nargs="+", help="serial capture(s) of the benchmark firmware, or its CSV")
        mp.add_argument("--board", required=True, help='e.g. "ST NUCLEO-L432KC"')
        mp.add_argument("--cpu", required=True, help='e.g. "Arm Cortex-M4F (STM32L432KC)"')
        mp.add_argument("--os", required=True, help='e.g. "Zephyr 4.2.0"')
        mp.add_argument("--rtlog", required=True, help="rtlog version measured, e.g. 0.2.4")
        mp.add_argument("--toolchain", required=True, help='e.g. "Zephyr SDK 0.17.2, GCC 12.2.0, -O2"')
        mp.add_argument("--target", required=True, help="Zephyr board target, e.g. nucleo_l432kc/stm32l432xx")
        mp.add_argument("--api", default="C header (rtlog.h) on Zephyr")
        mp.add_argument("--clock-mhz", type=float, help="core clock, if the capture has no RTLOG_BENCH line")
        mp.add_argument("--samples", default="256", help="calls per case, if the capture has no RTLOG_BENCH line")
        mp.add_argument("--notes", help="one paragraph appended to the report")
        mp.add_argument("--out", help="output file (default: stdout)")
        mcu_report(mp.parse_args(sys.argv[2:]))
        return 0
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="fail if results/README.md is not up to date")
    args = ap.parse_args()
    files = sorted(p for p in RESULTS.glob("*/*.md")) if RESULTS.is_dir() else []
    stray = [p for p in RESULTS.rglob("*") if p.is_file() and p != INDEX and p not in files] if RESULTS.is_dir() else []
    problems = [f"{p.relative_to(ROOT)}: results go in results/<version>/<board>.md" for p in stray]
    for f in files:
        problems += check(f)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    text = index(files)
    if args.check:
        if not INDEX.is_file() or INDEX.read_text(encoding="utf-8") != text:
            print("results/README.md is out of date: run `python3 scripts/results.py` and commit it", file=sys.stderr)
            return 1
        print(f"{len(files)} result file(s) OK; results/README.md is up to date")
        return 0
    RESULTS.mkdir(exist_ok=True)
    INDEX.write_text(text, encoding="utf-8")
    print(f"{len(files)} result file(s) OK; wrote {INDEX.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
