#!/usr/bin/env bash
# Stripped builds on Linux, end to end: build a Rust program (rtlog's `strip`
# feature) and the C demo (-DRTLOG_STRIP) with c/rtlog_strip.ld, read each
# contract from the linked binary, remove the descriptor sections, check that
# no format string is left in what ships (`rtlog verify`), run the stripped
# program and decode its capture. Fails on any mismatch.
#   scripts/strip_e2e.sh [cc]
# Needs GNU ld (-fuse-ld=bfd) and objcopy.
set -euo pipefail
CC=${1:-gcc}
cd "$(dirname "$0")/.."
cargo build -q -p rtlog-decode --bin rtlog
RTLOG=$PWD/target/debug/rtlog
OUT=$PWD/target/strip-e2e
rm -rf "$OUT" && mkdir -p "$OUT"
LD_SCRIPT=$PWD/c/rtlog_strip.ld
SECTIONS=(-R rtlog_sites -R rtlog_strs -R linkme_SITES -R linkm2_SITES)

# The library builds and lints with the feature, with and without std.
cargo clippy -q -p rtlog --features strip -- -D warnings
cargo clippy -q -p rtlog --no-default-features --features strip -- -D warnings

ship() { # name, linked binary: contract, strip, verify, run, decode
  local name=$1 linked=$2
  "$RTLOG" contract --elf "$linked" -o "$OUT/$name.contract.jsonl"
  # The linked binary still holds the strings, in sections that are not loaded.
  if "$RTLOG" verify --contract "$OUT/$name.contract.jsonl" "$linked" > "$OUT/$name.linked.txt" 2>&1; then
    echo "$name: verify passed on the unstripped binary"; exit 1
  fi
  objcopy "${SECTIONS[@]}" "$linked" "$OUT/$name.shipped"
  "$RTLOG" verify --contract "$OUT/$name.contract.jsonl" "$OUT/$name.shipped"
  "$OUT/$name.shipped" "$OUT/$name.rtlog"
  "$RTLOG" decode --contract "$OUT/$name.contract.jsonl" "$OUT/$name.rtlog" > "$OUT/$name.txt" 2> "$OUT/$name.err"
  # After stripping, the same program cannot give its contract any more.
  if "$RTLOG" contract --elf "$OUT/$name.shipped" > /dev/null 2>&1; then
    echo "$name: the shipped binary still has descriptors"; exit 1
  fi
  echo "$name: $(stat -c %s "$linked") -> $(stat -c %s "$OUT/$name.shipped") bytes, $(cat "$OUT/$name.err")"
}
check() { grep -q -- "$2" "$OUT/$1.txt" || { echo "missing in $1: $2"; head -40 "$OUT/$1.txt"; exit 1; }; }

# Rust. The flags apply to this build only, hence its own target directory.
RUSTFLAGS="-C link-arg=-fuse-ld=bfd -C link-arg=-Wl,-T,$LD_SCRIPT" \
  cargo build -q --release -p rtlog-examples --example stripped --features strip --target-dir "$OUT/target"
ship rust "$OUT/target/release/examples/stripped"
check rust "\[main\] INFO    stripped motor step=2 speed=1710 rpm"
check rust "\[main\] WARN  stripped temperature=81.5 C over limit=80"
check rust "◀ stripped.control_step"

# C: the demo, with the strings in rtlog_strs and both sections not loaded.
"$CC" -O2 -std=gnu11 -Wall -Wextra -Werror -pthread -DRTLOG_STRIP -I c/include c/examples/demo.c \
  -fuse-ld=bfd -Wl,-T,"$LD_SCRIPT" -o "$OUT/demo"
ship c "$OUT/demo"
check c "\[main\] INFO  demo start threads=2 version=0.1"
check c "\[sensor\] DEBUG   adc ch=3 raw=2000 volts=1.611"
check c "◀ control.tick"
echo "Stripped builds end-to-end OK ($CC)"
