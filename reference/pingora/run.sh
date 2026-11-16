#!/usr/bin/env bash
# Build the Pingora reference application and run it with each logger.
# Prints a markdown table; see README.md in this directory.
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

echo "| Access log | Requests/s | p50 µs | p99 µs | p99.9 µs | Log call, mean ns | Log bytes/request | Dropped |"
echo "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
# The variants take turns, and each round starts with another one, so that a
# change in the machine's load or the position in the round affects all of
# them alike.
orders=("none log rtlog" "rtlog none log" "log rtlog none")
for round in $(seq "$ROUNDS"); do
  for logger in ${orders[$(((round - 1) % 3))]}; do
    "$BIN" "$logger" "$OUT/$logger-$round" "$SECS" "$CONNS" "$THREADS" 2> "$OUT/$logger-$round.err"
  done
done

# Both logs hold the same records: one access line per request.
text=$(grep -c ' access method=GET path=/api/v1/items/' "$OUT/log-1/access.log")
"$RTLOG" decode --contract "$OUT/rtlog-1/access.contract.jsonl" "$OUT/rtlog-1/access.rtlog" > "$OUT/rtlog-1/access.txt"
binary=$(grep -c ' access method=GET path=/api/v1/items/' "$OUT/rtlog-1/access.txt")
if [ "$text" -le 1000 ] || [ "$binary" -le 1000 ]; then
  echo "too few access records: log $text, rtlog $binary"; exit 1
fi
echo
echo "First round: $text access records in the text log, $binary decoded from the rtlog capture."
