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

# Crash logging: the process really crashes; the dump must hold every record.
"$CC" -O1 -g -std=gnu11 -Wall -Wextra -Werror -pthread -I c/include c/examples/crash.c -o "$OUT/crash"
"$RTLOG" contract --elf "$OUT/crash" -o "$OUT/crash.contract.jsonl"
expect_crash() { # mode, expected exit status (128 + signal), expected text
  set +e
  rm -rf "$OUT/crash-$1" && mkdir -p "$OUT/crash-$1"
  "$OUT/crash" "$1" "$OUT/crash-$1" 2>/dev/null
  local st=$?
  set -e
  [ "$st" = "$2" ] || { echo "crash $1: exit $st, expected $2"; exit 1; }
  [ "$(ls "$OUT/crash-$1" | wc -l)" = 1 ] || { echo "crash $1: expected one dump"; ls "$OUT/crash-$1"; exit 1; }
  "$RTLOG" decode --contract "$OUT/crash.contract.jsonl" "$OUT/crash-$1"/crash-*-1.rtlog > "$OUT/crash-$1.txt"
  [ "$(grep -c 'before crash i=' "$OUT/crash-$1.txt")" = 500 ] || { echo "crash $1: records lost"; tail "$OUT/crash-$1.txt"; exit 1; }
  [ "$(grep -c 'worker step=' "$OUT/crash-$1.txt")" = 200 ] || { echo "crash $1: worker records lost"; exit 1; }
  tail -1 "$OUT/crash-$1.txt" | grep -q -- "$3" || { echo "crash $1: last line"; tail -2 "$OUT/crash-$1.txt"; exit 1; }
  echo "crash $1: $(tail -1 "$OUT/crash-$1.txt")"
}
case "$(uname -m)" in
  x86_64|i?86) expect_crash fpe 136 "CRASH SIGFPE" ;; # integer division by zero traps on x86 only
esac
expect_crash segv 139 "CRASH SIGSEGV"
expect_crash abort 134 "CRASH SIGABRT"
expect_crash snapshot 0 "SNAP  snapshot: watchdog: deadline missed"
# Thread slots are freed when threads exit: 10 x RTLOG_MAX_THREADS short threads, no drops.
"$OUT/crash" threads "$OUT/crash-threads-dir" || { echo "thread slots were not released"; exit 1; }
echo "C and C++ end-to-end OK ($CC, $CXX)"
