#!/bin/sh
# usage: burst-minute.sh OUT RESCAN_MS PER_SECOND COLLECTOR [COLLECTOR_ARGS...]
# Runs the collector for one minute and, 15 s in, followprobe at RESCAN_MS
# with a 30 s burst of PER_SECOND forks from two generators. Writes both
# reports to OUT after the date, load average and power source.
set -eu
out=$1 rescan_ms=$2 per_second=$3
shift 3
bin=$(dirname "$0")/../../target/release

{
  date
  sysctl -n vm.loadavg
  pmset -g batt | head -1
} > "$out"
"$@" --seconds 60 > "$out.collector" 2>&1 &
collector=$!
sleep 15
"$bin/followprobe" "$rescan_ms" "$per_second" 30 6 2 > "$out.probe" 2>&1
wait "$collector"
cat "$out.collector" "$out.probe" >> "$out"
rm "$out.collector" "$out.probe"
