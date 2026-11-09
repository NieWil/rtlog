#!/bin/sh
# rtlog demo: measure the logger on this machine and trace a small app.
# Runs from the unpacked rtlog-demo package; POSIX sh, no other dependency.
#
#   ./run.sh [output-dir]       (default: ./results-<hostname>)
#
# Writes into the output directory:
#   timing.md               cost per log call in ns and cycles, vs formatting
#   quickstart.perfetto.json  4 threads with spans, open in ui.perfetto.dev
#   app/                    a traced app: raw stream, contract, text, JSON,
#                           Perfetto trace
#   stdout.txt              the stdout mode: text printed as the app runs
#   system.txt              CPU, kernel, architecture
set -eu

here=$(cd "$(dirname "$0")" && pwd)
host=$(hostname 2>/dev/null || uname -n)
out=${1:-"$PWD/results-$host"}
mkdir -p "$out"

step() { printf '\n== %s\n' "$*"; }

step "system"
{
    echo "host: $host"
    echo "uname: $(uname -a)"
    if [ -r /proc/cpuinfo ]; then
        grep -m1 -E '^(model name|Hardware|cpu model|uarch)' /proc/cpuinfo || true
        echo "cpus: $(grep -c ^processor /proc/cpuinfo)"
    fi
    command -v sysctl >/dev/null 2>&1 && sysctl -n machdep.cpu.brand_string 2>/dev/null || true
} | tee "$out/system.txt"

step "timing: cost per log call (a few seconds)"
# RTLOG_BOARD names the board in the report (default: what the system reports).
"$here/bin/timing" --out "$out/timing.md" >/dev/null
# The headline rows: platform, clock read, W1..W3, the formatting baselines.
grep -E '^\| (CPU|Measured core clock|Workload|Clock read|W[1-5]|Disabled|Baseline|W1 speed-up)' "$out/timing.md" || true
grep -E '^Spec budget' "$out/timing.md" || true

step "perfetto quickstart"
"$here/bin/perfetto_quickstart" "$out/quickstart.perfetto.json"

step "traced app (control loop, workers, deadlock watchdog)"
"$here/bin/app_trace" "$out/app"

step "stdout mode"
RTLOG=${RTLOG:-debug} "$here/bin/stdout_mode" >"$out/stdout.txt" 2>&1 || true
head -n 8 "$out/stdout.txt"

step "decode the raw capture again with the CLI (what you do on the host)"
"$here/bin/rtlog" decode --contract "$out/app/app.contract.jsonl" "$out/app/app.rtlog" | head -n 5

cat <<EOF

Done. Results in $out
  timing.md                 read it, compare it with other boards, and share it:
                            https://github.com/NieWil/rtlog/blob/main/CONTRIBUTING.md#share-your-boards-results
                            (name your board with RTLOG_BOARD="..." ./run.sh)
  quickstart.perfetto.json  open in https://ui.perfetto.dev (Open trace file)
  app/app.perfetto.json     the traced app in Perfetto
Copy the directory to your computer, for example:
  scp -r $(whoami 2>/dev/null || echo user)@$host:$out .
EOF
