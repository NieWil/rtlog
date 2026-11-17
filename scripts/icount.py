#!/usr/bin/env python3
"""Instructions per log call, counted with Valgrind's callgrind.

Builds examples/icount.rs (release) and runs each of its workloads under
callgrind with collection switched on only inside the workload's function.
The result is the same on every run of the same binary, whatever else the
machine is doing, so it can gate a pull request where timings cannot.

usage: scripts/icount.py [--out report.md] [--bmf icount.json]

--bmf writes Bencher Metric Format JSON for `bencher run --adapter json`
(.github/workflows/bench.yml). Needs valgrind.
"""
import argparse
import json
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
# Keep in step with examples/icount.rs.
CALLS = 50 * 1000
WORKLOADS = [
    ("w1", "icount_w1", '`info!(to: p, "x={} y={}", u32, u32)`, explicit producer'),
    ("noargs", "icount_noargs", '`info!(to: p, "tick")`, no argument'),
    ("str", "icount_str", '`info!(to: p, "state={}", &str)`, 7-byte string'),
    ("disabled", "icount_disabled", "`debug!` below the runtime level"),
    ("span", "icount_span", "`span!` opened and closed (two frames)"),
    ("local_w1", "icount_local_w1", '`info!("x={} y={}", u32, u32)`, thread-local ring (`std`)'),
]


def instructions(binary, arg, function, tmp):
    out = pathlib.Path(tmp) / ("callgrind.%s" % arg)
    run = subprocess.run(
        ["valgrind", "--tool=callgrind", "--callgrind-out-file=%s" % out,
         "--toggle-collect=%s" % function, str(binary), arg],
        capture_output=True, text=True)
    if run.returncode != 0:
        sys.exit("valgrind failed for %s:\n%s" % (arg, run.stderr))
    if " 0 dropped" not in run.stdout:
        sys.exit("%s: records were dropped: %s" % (arg, run.stdout.strip()))
    total = re.search(r"^(?:totals|summary):\s*(\d+)", out.read_text(), re.M)
    if not total or int(total.group(1)) == 0:
        sys.exit("%s: callgrind counted nothing inside %s" % (arg, function))
    return int(total.group(1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out")
    ap.add_argument("--bmf")
    a = ap.parse_args()
    subprocess.check_call(["cargo", "build", "-q", "--release", "-p", "rtlog-examples", "--example", "icount"],
                          cwd=ROOT)
    binary = ROOT / "target" / "release" / "examples" / "icount"
    rustc = subprocess.check_output(["rustc", "--version"], text=True).strip()
    lines = [
        "## rtlog instructions per log call",
        "",
        "`examples/icount.rs`, release build, %s, counted with Valgrind's callgrind inside the workload "
        "function only (%d calls each, loop included). Instructions executed, not cycles or time: "
        "the same on every run of the same binary." % (rustc, CALLS),
        "",
        "| Workload | Call | Instructions per call |",
        "| --- | --- | ---: |",
    ]
    bmf = {}
    with tempfile.TemporaryDirectory() as tmp:
        for arg, function, what in WORKLOADS:
            per_call = instructions(binary, arg, function, tmp) / CALLS
            lines.append("| %s | %s | %.1f |" % (arg, what, per_call))
            bmf["log call %s" % arg] = {"instructions": {"value": round(per_call, 2)}}
    report = "\n".join(lines) + "\n"
    print(report)
    if a.out:
        pathlib.Path(a.out).write_text(report)
    if a.bmf:
        pathlib.Path(a.bmf).write_text(json.dumps(bmf, indent=2) + "\n")


if __name__ == "__main__":
    sys.exit(main())
