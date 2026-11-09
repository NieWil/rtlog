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
  [ "$(find "$OUT/crash-$1" -type f | wc -l)" = 1 ] || { echo "crash $1: expected one dump"; ls "$OUT/crash-$1"; exit 1; }
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
# Two threads fault at once: one complete dump (the second waits for the first).
expect_crash two 139 "CRASH SIGSEGV"
# A dump never overwrites an existing file, even one with the same pid.
rm -rf "$OUT/collide" && mkdir -p "$OUT/collide"
"$OUT/crash" collide "$OUT/collide" || { echo "collide: exit $?"; exit 1; }
[ "$(find "$OUT/collide" -type f | wc -l)" = 2 ] || { echo "collide: expected 2 files"; ls "$OUT/collide"; exit 1; }
[ "$(cat "$OUT"/collide/crash-*-1.rtlog)" = "earlier crash" ] || { echo "collide: earlier file was overwritten"; exit 1; }
# A flood of snapshots is capped (16), then a fatal signal is still dumped: 17 files.
rm -rf "$OUT/flood" && mkdir -p "$OUT/flood"
set +e; "$OUT/crash" flood "$OUT/flood" 2>/dev/null; st=$?; set -e
[ "$st" = 139 ] || { echo "flood: exit $st"; exit 1; }
[ "$(find "$OUT/flood" -type f | wc -l)" = 17 ] || { echo "flood: expected 17 files"; find "$OUT/flood" -type f | wc -l; exit 1; }
# The drain writes resync frames; a capture with damage in the middle still decodes
# past it (the decoder skips to the next resync frame).
rm -rf "$OUT/resync" && mkdir -p "$OUT/resync"
"$OUT/crash" resync "$OUT/resync" || { echo "resync: capture failed"; exit 1; }
"$RTLOG" decode --contract "$OUT/crash.contract.jsonl" "$OUT/resync/resync.rtlog" 2>"$OUT/resync/clean.err" >/dev/null \
  || { echo "resync: clean capture does not decode"; cat "$OUT/resync/clean.err"; exit 1; }
grep -q "12000 records" "$OUT/resync/clean.err" || { echo "resync: expected 12000 records"; cat "$OUT/resync/clean.err"; exit 1; }
[ "$(LC_ALL=C grep -c -a -o 'RTL1' "$OUT/resync/resync.rtlog")" -ge 3 ] || { echo "resync: no resync frames"; exit 1; }
printf '\377%.0s' $(seq 80) | dd of="$OUT/resync/resync.rtlog" bs=1 seek=100000 conv=notrunc 2>/dev/null
"$RTLOG" decode --contract "$OUT/crash.contract.jsonl" "$OUT/resync/resync.rtlog" 2>"$OUT/resync/bad.err" >/dev/null \
  || { echo "resync: damaged capture did not recover"; cat "$OUT/resync/bad.err"; exit 1; }
grep -q "stream gap" "$OUT/resync/bad.err" || { echo "resync: no gap reported"; cat "$OUT/resync/bad.err"; exit 1; }
echo "resync: $(tail -1 "$OUT/resync/bad.err")"
# Thread slots are freed when threads exit: 10 x RTLOG_MAX_THREADS short threads, no drops.
"$OUT/crash" threads "$OUT/crash-threads-dir" || { echo "thread slots were not released"; exit 1; }
echo "C and C++ end-to-end OK ($CC, $CXX)"
