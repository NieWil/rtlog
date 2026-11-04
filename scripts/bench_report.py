#!/usr/bin/env python3
"""Summarise Criterion results (target/criterion/**/new/estimates.json) as markdown."""
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/criterion")
rows = []
for est in sorted(root.glob("**/new/estimates.json")):
    bench_dir = est.parent.parent
    name = str(bench_dir.relative_to(root))
    data = json.loads(est.read_text())
    mean = data["mean"]
    bench = bench_dir / "new" / "benchmark.json"
    per = ""
    if bench.exists():
        tp = json.loads(bench.read_text()).get("throughput") or {}
        n = tp.get("Elements")
        if n:
            per = f"{mean['point_estimate'] / n:.1f}"
    lo = mean["confidence_interval"]["lower_bound"]
    hi = mean["confidence_interval"]["upper_bound"]
    rows.append(f"| `{name}` | {mean['point_estimate']:.1f} | {lo:.1f} – {hi:.1f} | {per} |")

print("## Criterion benchmarks\n")
if not rows:
    print("No results found.")
else:
    print("| Benchmark | Mean (ns) | 95% CI (ns) | ns per element |")
    print("| --- | ---: | ---: | ---: |")
    print("\n".join(rows))
