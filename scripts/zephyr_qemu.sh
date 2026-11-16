#!/usr/bin/env bash
# Build the Zephyr hello sample for qemu_cortex_m3 with rtlog as an extra
# module, run it in QEMU with the console UART captured raw, extract the
# contract from zephyr.elf and decode the capture. Fails on any mismatch.
#   scripts/zephyr_qemu.sh [extra west build args...] [-DNAME=VALUE...] [-- cmake args...]
# Needs `west` on PATH, a Zephyr workspace (ZEPHYR_BASE, or the current
# directory inside one) and the Zephyr SDK with arm-zephyr-eabi.
#   RTLOG_QEMU_SECS  seconds QEMU runs before it is stopped (default 5)
#   RTLOG_QEMU_NAME  build directory target/zephyr-qemu/<name> (default hello)
#   RTLOG_BIN        rtlog CLI to use instead of building target/debug/rtlog
#   QEMU             qemu-system-arm to use instead of the one Zephyr found
set -euo pipefail
SECS=${RTLOG_QEMU_SECS:-5}
NAME=${RTLOG_QEMU_NAME:-hello}

# Arguments before `--` go to west build, except -D..., which go to CMake
# together with everything after `--`.
west_args=()
cmake_args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; cmake_args+=("$@"); break ;;
    -D*) cmake_args+=("$1") ;;
    *) west_args+=("$1") ;;
  esac
  shift
done

if [ -z "${ZEPHYR_BASE:-}" ]; then
  west topdir >/dev/null || { echo "set ZEPHYR_BASE or run inside a west workspace"; exit 1; }
  ZEPHYR_BASE=$(west list -f '{abspath}' zephyr)
  export ZEPHYR_BASE
fi

cd "$(dirname "$0")/.."
if [ -n "${RTLOG_BIN:-}" ]; then
  RTLOG=$RTLOG_BIN
else
  cargo build -q -p rtlog-decode --bin rtlog
  RTLOG=$PWD/target/debug/rtlog
fi
OUT=target/zephyr-qemu/$NAME
mkdir -p "$OUT"

# CONFIG_BOOT_BANNER selects CONFIG_PRINTK, so the sample's CONFIG_PRINTK=n has
# no effect while the banner is on, and the banner text would precede the
# stream header on the UART. QEMU boards build no zephyr.bin by default.
west build -b qemu_cortex_m3 -d "$OUT" "${west_args[@]}" c/zephyr/samples/hello -- \
  -DZEPHYR_EXTRA_MODULES="$PWD" -DCONFIG_BOOT_BANNER=n -DCONFIG_BUILD_OUTPUT_BIN=y \
  "${cmake_args[@]}" > "$OUT/build.log" 2>&1 || { tail -40 "$OUT/build.log"; echo "west build failed, see $OUT/build.log"; exit 1; }
ELF=$OUT/zephyr/zephyr.elf

cache() { sed -n "s/^$1:[A-Z]*=//p" "$OUT/CMakeCache.txt"; }
QEMU=${QEMU:-$(cache QEMU)}
QEMU=${QEMU:-qemu-system-arm}
SIZE=$(cache CMAKE_OBJDUMP)
SIZE=${SIZE%objdump}size

# The machine arguments of `west build -t run`, with the UART in a file
# instead of on stdio (binary safe). timeout stops QEMU: status 124.
rm -f "$OUT/capture.rtlog"
set +e
timeout "$SECS" "$QEMU" -cpu cortex-m3 -machine lm3s6965evb -nographic -monitor none -net none \
  -serial "file:$OUT/capture.rtlog" -kernel "$ELF" < /dev/null > "$OUT/qemu.log" 2>&1
st=$?
set -e
[ "$st" = 124 ] || { echo "qemu: exit $st, expected 124 (stopped by timeout)"; cat "$OUT/qemu.log"; exit 1; }

"$RTLOG" contract --elf "$ELF" -o "$OUT/app.contract.jsonl"
set +e
"$RTLOG" decode --contract "$OUT/app.contract.jsonl" "$OUT/capture.rtlog" > "$OUT/app.txt" 2> "$OUT/app.err"
st=$?
set -e
if [ "$st" != 0 ]; then
  # QEMU is stopped at an arbitrary point, so the last frame can be cut off.
  # Tolerate only that: one "stopped early ... truncated" error whose offset is
  # in the last 64 bytes of the capture. Anything else fails.
  size=$(stat -c %s "$OUT/capture.rtlog")
  off=$(sed -n 's/.*: stopped early: offset \([0-9]*\): bad frame: [Tt]runcated.*/\1/p' "$OUT/app.err")
  if [ "$(grep -c 'stopped early' "$OUT/app.err")" = 1 ] && [ -n "$off" ] && [ "$((size - off))" -le 64 ]; then
    echo "note: last frame cut off when QEMU stopped (offset $off of $size bytes)"
  else
    echo "decode: exit $st"; cat "$OUT/app.err"; exit 1
  fi
fi

check() { grep -q -- "$1" "$OUT/app.txt" || { echo "missing: $1"; head -40 "$OUT/app.txt"; cat "$OUT/app.err"; exit 1; }; }
check "\[main\] INFO    tick=[0-9]* uptime_ms=[0-9]*"
check "\[sensor\] DEBUG   adc ch=3 raw=2[0-9][0-9][0-9]"
check "\[main\] SPAN  ▶ control.tick"
check "\[main\] SPAN  ◀ control.tick"
check "\[sensor\] SPAN  ▶ adc.read"
check "\[sensor\] SPAN  ◀ adc.read"
summary=$(grep ' records, ' "$OUT/app.err")
records=${summary%% records,*}
[ "$records" -gt 100 ] || { echo "expected more than 100 records: $summary"; exit 1; }

"$SIZE" -A "$ELF" | grep -E '^(section|text|rodata|rtlog_[a-z_]*|log_strings) '
bin=$(stat -c %s "$OUT/zephyr/zephyr.bin")
sites=$("$SIZE" -A "$ELF" | awk '$1 == "rtlog_sites" {print $2}')
# What is flashed holds every format string, or with CONFIG_RTLOG_STRIP=y none.
stripped="format strings in zephyr.bin"
if grep -q '^CONFIG_RTLOG_STRIP=y' "$OUT/zephyr/.config"; then
  "$RTLOG" verify --contract "$OUT/app.contract.jsonl" "$OUT/zephyr/zephyr.bin" > "$OUT/verify.txt" 2>&1 \
    || { cat "$OUT/verify.txt"; exit 1; }
  stripped="stripped: no format string in zephyr.bin"
elif "$RTLOG" verify --contract "$OUT/app.contract.jsonl" "$OUT/zephyr/zephyr.bin" > "$OUT/verify.txt" 2>&1; then
  echo "verify passed on an image that is not stripped"; cat "$OUT/verify.txt"; exit 1
fi
echo "Zephyr QEMU end-to-end OK ($NAME, ${SECS} s): $summary; zephyr.bin $bin bytes, rtlog_sites $sites bytes, $stripped"
