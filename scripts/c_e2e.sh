#!/usr/bin/env bash
# Build the C and C++ examples, run them, extract their contracts from the
# binaries and decode the captures. Fails on any mismatch.
#   scripts/c_e2e.sh [cc] [c++]
set -euo pipefail
CC=${1:-cc}
CXX=${2:-c++}
cd "$(dirname "$0")/.."
cargo build -q -p rtlog-decode --bin rtlog
RTLOG=$PWD/target/debug/rtlog
OUT=target/c-e2e/$(basename "$CC")
mkdir -p "$OUT"

"$CC" -O2 -std=gnu11 -Wall -Wextra -Werror -pthread -I c/include c/examples/demo.c -o "$OUT/demo"
"$CXX" -O2 -std=c++17 -Wall -Wextra -Werror -pthread -I c/include c/examples/demo.cpp -o "$OUT/demo_cpp"

for app in demo demo_cpp; do
  "$OUT/$app" "$OUT/$app.rtlog"
  "$RTLOG" contract --elf "$OUT/$app" -o "$OUT/$app.contract.jsonl"
  "$RTLOG" decode --contract "$OUT/$app.contract.jsonl" "$OUT/$app.rtlog" > "$OUT/$app.txt"
  "$RTLOG" decode --contract "$OUT/$app.contract.jsonl" --perfetto -o "$OUT/$app.perfetto.json" "$OUT/$app.rtlog"
done

check() { grep -q -- "$2" "$OUT/$1.txt" || { echo "missing in $1: $2"; cat "$OUT/$1.txt" | head -40; exit 1; }; }
check demo "\[main\] INFO  demo start threads=2 version=0.1"
check demo "\[sensor\] DEBUG   adc ch=3 raw=2000 volts=1.611"
check demo "\[control\] ERROR motor fault code=-7 name=overcurrent"
check demo "◀ control.tick"
check demo_cpp "INFO    request id=0 route=/health"
check demo_cpp "DEBUG     rows=3 cached=false"
check demo_cpp "WARN  shutdown pending=0 ok=true"
test "$(grep -c 'INFO  demo\|INFO    request' "$OUT/demo.txt" "$OUT/demo_cpp.txt" | awk -F: '{s+=$2} END {print s}')" -ge 32
echo "C and C++ end-to-end OK ($CC, $CXX)"
