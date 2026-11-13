#!/usr/bin/env bash
# The real renderer drawing as fast as it can, with no window, at several view distances.
#
#   tools/bench-client.sh [VIEW_DISTANCE...]      (default: 8 16 32)
#
# Each run makes a new world, waits until everything in view is on the GPU, timing how long that
# takes, then reads the last 240 frames' times from the game itself. BENCH_FLAGS passes more options
# to the game, such as BENCH_FLAGS=--ray-tracing. Build first: cargo build --release.
set -u
cd "$(dirname "$0")/.." || exit 1
distances=("$@")
[ ${#distances[@]} -eq 0 ] && distances=(8 16 32)
ctl() { timeout 60 target/release/zenith-ctl --endpoint "$endpoint" "$@"; }
report() { ctl frame 2>/dev/null | grep "\"$1\"" | head -1 | sed 's/.*: *//; s/,$//'; }
average() { ctl frame 2>/dev/null | grep -A1 "\"$1\"" | grep average | head -1 | sed 's/.*: *//; s/,$//'; }
p99() { ctl frame 2>/dev/null | grep -A2 "\"$1\"" | grep p99 | head -1 | sed 's/.*: *//; s/,$//'; }
export ZENITH_NO_AUDIO=1 MSYS_NO_PATHCONV=1

echo "| view distance | time to a complete view | fps | frame ms (average) | frame ms (p99) | CPU ms a frame | sections drawn | quads | vertex MB |"
echo "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
for view in "${distances[@]}"; do
  home=$(mktemp -d)
  # Under Git Bash, a /tmp path means one place to bash and another to a Windows program.
  command -v cygpath >/dev/null && home=$(cygpath -m "$home")
  endpoint="$home/control.json"
  export ZENITH_HOME="$home"
  started=$(date +%s.%N)
  # shellcheck disable=SC2086 # BENCH_FLAGS is a list of options
  target/release/zenith --offscreen --uncapped --width 1920 --height 1080 --render-distance "$view" ${BENCH_FLAGS:-} \
    --world bench --seed 2026 --creative --control "$endpoint" >"$home/game.log" 2>&1 &
  for _ in $(seq 1 240); do
    [ -f "$endpoint" ] && ctl frame >/dev/null 2>&1 && break
    sleep 0.25
  done
  ctl wait-ready 120 >/dev/null
  ctl look 200 8 >/dev/null
  for _ in $(seq 1 1200); do
    [ "$(report chunks_loaded)" -ge "$(report chunks_wanted)" ] && [ "$(report meshes_pending)" = 0 ] &&
      [ "$(report far_pending)" = 0 ] && [ "$(report traced_waiting)" = 0 ] && break
    sleep 0.1
  done
  settled=$(awk "BEGIN { print $(date +%s.%N) - $started }")
  sleep 8
  printf "| %s | %.1f s | %.0f | %.2f | %.2f | %.2f | %s | %s | %.0f |\n" "$view" "$settled" "$(report fps)" \
    "$(average frame_ms)" "$(p99 frame_ms)" "$(average cpu_ms)" "$(report sections_drawn)" "$(report quads)" "$(report vertex_mb_used)"
  ctl quit >/dev/null
  sleep 2
  rm -rf "$home"
done
