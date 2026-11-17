#!/usr/bin/env bash
# Build the Pingora reference application and run it with each logger.
# Prints two markdown tables (timings; instructions per access-log call, if
# valgrind is installed); see README.md in this directory.
#   reference/pingora/run.sh [seconds] [connections] [proxy-threads] [rounds]
#   PINGORA_BMF=file  also write the results as Bencher Metric Format JSON
set -euo pipefail
SECS=${1:-10}
CONNS=${2:-32}
THREADS=${3:-4}
ROUNDS=${4:-3}
cd "$(dirname "$0")"
cargo build -q --release
(cd ../.. && cargo build -q -p rtlog-decode --bin rtlog)
RTLOG=$PWD/../../target/debug/rtlog
BIN=$PWD/target/release/rtlog-reference-pingora
OUT=$PWD/target/run
rm -rf "$OUT" && mkdir -p "$OUT"

echo "| Access log | Requests/s | Request p50 µs | p99 µs | p99.9 µs | Log call ns: mean | p50 | p99 | p99.9 | longest | Log bytes/request | Peak RSS MiB | Dropped |"
echo "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
# The variants take turns, and each round starts with another one, so that a
# change in the machine's load or the position in the round affects all of
# them alike.
variants=(none log log-buf tracing rtlog)
for round in $(seq "$ROUNDS"); do
  for i in 0 1 2 3 4; do
    logger=${variants[$(((i + round - 1) % 5))]}
    "$BIN" "$logger" "$OUT/$logger-$round" "$SECS" "$CONNS" "$THREADS" 2> "$OUT/$logger-$round.err" | grep '^|' | tee -a "$OUT/rows.md"
  done
done

# Instructions of the access-log call, counted with callgrind: 20 000 calls
# with fixed fields from one thread, no proxy. The same on every run, unlike
# the timings above.
if command -v valgrind > /dev/null; then
  echo
  echo "| Access log | Instructions per access-log call |"
  echo "| --- | ---: |"
  for logger in log log-buf tracing rtlog; do
    valgrind --tool=callgrind --callgrind-out-file="$OUT/callgrind.$logger" --toggle-collect=pingora_access_log \
      "$BIN" icount "$logger" "$OUT/icount-$logger" > "$OUT/icount-$logger.txt" 2> "$OUT/icount-$logger.err"
    total=$(sed -n 's/^totals: *//p' "$OUT/callgrind.$logger")
    calls=$(sed -n 's/^access_log_calls \([0-9]*\) dropped 0$/\1/p' "$OUT/icount-$logger.txt")
    if [ -z "$total" ] || [ -z "$calls" ]; then
      echo "callgrind run of $logger failed: totals '$total', $(cat "$OUT/icount-$logger.txt")"; exit 1
    fi
    echo "| $logger | $((total / calls)) |" | tee -a "$OUT/icount.md"
  done
else
  echo
  echo "valgrind not found: instruction counts skipped."
fi

# Bencher Metric Format JSON (.github/workflows/bench.yml): the instruction
# counts, and per logger the means over the rounds of what a shared machine
# can only indicate.
if [ -n "${PINGORA_BMF:-}" ]; then
  python3 - "$OUT/rows.md" "$OUT/icount.md" "$PINGORA_BMF" << 'PY'
import json, pathlib, sys
rows, icount, out = (pathlib.Path(p) for p in sys.argv[1:])
bmf, runs = {}, {}
for line in rows.read_text().splitlines():
    c = [x.strip() for x in line.strip("|").split("|")]
    runs.setdefault(c[0], []).append([float(x) for x in c[1:]])
for logger, r in runs.items():
    mean = [sum(col) / len(col) for col in zip(*r)]
    bmf["pingora access log %s" % logger] = {
        "throughput": {"value": mean[0]},
        "log-call-p99-ns": {"value": mean[6]},
        "log-call-longest-ns": {"value": max(x[8] for x in r)},
        "peak-rss-mib": {"value": mean[10]},
    }
if icount.exists():
    for line in icount.read_text().splitlines():
        c = [x.strip() for x in line.strip("|").split("|")]
        bmf.setdefault("pingora access log %s" % c[0], {})["instructions"] = {"value": float(c[1])}
out.write_text(json.dumps(bmf, indent=2) + "\n")
PY
fi

# Both logs hold the same records: one access line per request.
text=$(grep -c ' access method=GET path=/api/v1/items/' "$OUT/log-1/access.log")
"$RTLOG" decode --contract "$OUT/rtlog-1/access.contract.jsonl" "$OUT/rtlog-1/access.rtlog" > "$OUT/rtlog-1/access.txt"
binary=$(grep -c ' access method=GET path=/api/v1/items/' "$OUT/rtlog-1/access.txt")
if [ "$text" -le 1000 ] || [ "$binary" -le 1000 ]; then
  echo "too few access records: log $text, rtlog $binary"; exit 1
fi
echo
echo "First round: $text access records in the text log, $binary decoded from the rtlog capture."
