#!/usr/bin/env bash
# Takes pictures of a world from given places, with no window. The pictures in docs/media come from it.
#
#   tools/tour.sh OUT_DIR SEED STOP...
#
# A STOP is "name x y z yaw pitch time" for a picture, "/command" for a chat command, "key:NAME" to
# press a key (key:f1 brings the HUD back, which starts hidden), "click:X,Y" or "rclick:X,Y" to click a
# pixel of the picture with the left or right button, "mouse:X,Y" to point at one, or "shot:name" for a
# picture from wherever the camera is, "type:TEXT" to type, or "ctl:COMMAND ARGS" for any other
# zenith-ctl command. TOUR_FLAGS passes more options
# to the game, such as TOUR_FLAGS=--ray-tracing. Build first: cargo build --release.
set -u
cd "$(dirname "$0")/.." || exit 1
out=$1
seed=$2
shift 2
mkdir -p "$out"
out=$(cd "$out" && pwd)
command -v cygpath >/dev/null && out=$(cygpath -m "$out")
home=$(mktemp -d)
# Under Git Bash, a /tmp path means one place to bash and another to a Windows program.
command -v cygpath >/dev/null && home=$(cygpath -m "$home")
endpoint="$home/control.json"
ctl() { timeout 60 target/release/zenith-ctl --endpoint "$endpoint" "$@"; }
# A number from the game's `frame` report.
report() { ctl frame 2>/dev/null | grep "\"$1\"" | head -1 | tr -dc '0-9'; }
export ZENITH_HOME="$home" ZENITH_NO_AUDIO=1 MSYS_NO_PATHCONV=1

# shellcheck disable=SC2086 # TOUR_FLAGS is a list of options
target/release/zenith --offscreen --width 1600 --height 900 --render-distance 16 ${TOUR_FLAGS:-} \
  --world tour --seed "$seed" --creative --control "$endpoint" >"$home/game.log" 2>&1 &
for _ in $(seq 1 120); do
  [ -f "$endpoint" ] && ctl frame >/dev/null 2>&1 && break
  sleep 0.5
done
ctl wait-ready 60 >/dev/null
ctl fly >/dev/null
ctl key f1 >/dev/null

for stop in "$@"; do
  case "$stop" in
    /*) ctl chat "$stop" >/dev/null; sleep 0.5; continue ;;
    key:*) ctl key "${stop#key:}" >/dev/null; sleep 0.5; continue ;;
    type:*) ctl type "${stop#type:}" >/dev/null; sleep 0.3; continue ;;
    # shellcheck disable=SC2086 # the rest is a command and its arguments
    ctl:*) ctl ${stop#ctl:} >/dev/null; sleep 0.5; continue ;;
    click:*) at=${stop#click:}; ctl mouse "${at%,*}" "${at#*,}" --left >/dev/null; sleep 0.5; continue ;;
    rclick:*) at=${stop#rclick:}; ctl mouse "${at%,*}" "${at#*,}" --right >/dev/null; sleep 0.5; continue ;;
    mouse:*) at=${stop#mouse:}; ctl mouse "${at%,*}" "${at#*,}" >/dev/null; sleep 0.5; continue ;;
    shot:*)
      sleep 1
      if ctl screenshot "$out/${stop#shot:}.png" >/dev/null; then echo "${stop#shot:}"; else echo "${stop#shot:} FAILED"; fi
      continue ;;
  esac
  read -r name x y z yaw pitch time <<<"$stop"
  ctl chat "/tp $x $y $z" >/dev/null
  ctl chat "/time set $time" >/dev/null
  ctl look "$yaw" "$pitch" >/dev/null
  # Until everything in view has arrived, and a little longer for it to settle.
  sleep 2
  for _ in $(seq 1 120); do
    [ "$(report chunks_loaded)" -ge "$(report chunks_wanted)" ] && [ "$(report meshes_pending)" = 0 ] &&
      [ "$(report far_pending)" = 0 ] && [ "$(report traced_waiting)" = 0 ] && break
    sleep 0.5
  done
  sleep 3
  if ctl screenshot "$out/$name.png" >/dev/null; then echo "$name"; else echo "$name FAILED"; fi
done
ctl quit >/dev/null
sleep 2
rm -rf "$home"
