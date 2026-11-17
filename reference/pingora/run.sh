#!/usr/bin/env bash
# Build the Pingora reference application and run it with each logger.
# Prints two markdown tables (timings; instructions per access-log call, if
# valgrind is installed); see README.md in this directory.
#   reference/pingora/run.sh [seconds] [connections] [proxy-threads] [rounds]
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

echo "| Access log | Requests/s | p50 µs | p99 µs | p99.9 µs | Log call, mean ns | Log bytes/request | Peak RSS MiB | Dropped |"
echo "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
# The variants take turns, and each round starts with another one, so that a
# change in the machine's load or the position in the round affects all of
# them alike.
variants=(none log log-buf tracing rtlog)
for round in $(seq "$ROUNDS"); do
  for i in 0 1 2 3 4; do
    logger=${variants[$(((i + round - 1) % 5))]}
    "$BIN" "$logger" "$OUT/$logger-$round" "$SECS" "$CONNS" "$THREADS" 2> "$OUT/$logger-$round.err" | grep '^|'
  done
done

# Instructions executed inside the access-log call on the worker thread,
# counted with callgrind: the same on every run, unlike the timings above.
# One proxy thread and a light load: the count per call does not depend on it.
if command -v valgrind > /dev/null; then
  echo
  echo "| Access log | Instructions per access-log call (worker thread) |"
  echo "| --- | ---: |"
  for logger in log log-buf tracing rtlog; do
    valgrind --tool=callgrind --callgrind-out-file="$OUT/callgrind.$logger" --toggle-collect=pingora_access_log \
      "$BIN" "$logger" "$OUT/cg-$logger" 4 4 1 > "$OUT/cg-$logger.txt" 2> "$OUT/cg-$logger.err"
    total=$(sed -n 's/^totals: *//p' "$OUT/callgrind.$logger")
    calls=$(sed -n 's/^access_log_calls //p' "$OUT/cg-$logger.txt")
    if [ -z "$total" ] || [ -z "$calls" ] || [ "$calls" -lt 500 ]; then
      echo "callgrind run of $logger failed: totals '$total', calls '$calls'"; exit 1
    fi
    echo "| $logger | $((total / calls)) |"
  done
else
  echo
  echo "valgrind not found: instruction counts skipped."
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
