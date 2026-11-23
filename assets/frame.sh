#!/usr/bin/env bash
# Grab screenshots from this unit's source videos.
#
#   scripts/frame.sh <video> <time>                 one frame, e.g. 03:10
#   scripts/frame.sh <video> <from> <to> [step]     a frame every [step] seconds (default 5)
#
# <video> is a file name listed in sources.md, or any path. Frames go to ./frames/.
set -euo pipefail

VIDEO_DIR='__VIDEO_DIR__'

if [ $# -lt 2 ]; then
  sed -n '2,8p' "$0"; exit 1
fi
command -v ffmpeg >/dev/null || { echo "ffmpeg is required" >&2; exit 1; }

video="$1"
if [ ! -f "$video" ]; then
  video="$VIDEO_DIR/$1"
fi
if [ ! -f "$video" ]; then
  video="$(find "$VIDEO_DIR" -type f -name "$(basename "$1")" 2>/dev/null | head -n 1)"
fi
[ -n "$video" ] && [ -f "$video" ] || { echo "video not found: $1 (looked under $VIDEO_DIR)" >&2; exit 1; }

to_secs() { awk -F: '{ s=0; for (i=1;i<=NF;i++) s=s*60+$i; print s }' <<<"$1"; }

out_dir="${FRAMES_DIR:-./frames}"
mkdir -p "$out_dir"
base="$(basename "${video%.*}")"

shot() {
  local t="$1" label out
  label="$(awk '{ printf "%02d-%02d-%03d", int($1/60), int($1%60), int(($1-int($1))*1000) }' <<<"$t")"
  out="$out_dir/${base}_${label}.jpg"
  ffmpeg -nostdin -y -v error -ss "$t" -i "$video" -frames:v 1 -q:v 2 "$out"
  echo "$out"
}

from="$(to_secs "$2")"
if [ $# -ge 3 ]; then
  to="$(to_secs "$3")"; step="${4:-5}"
  awk -v s="$step" 'BEGIN{exit !(s>0)}' || { echo "step must be greater than 0" >&2; exit 1; }
  awk -v a="$from" -v b="$to" 'BEGIN{exit !(b>=a)}' || { echo "<to> must not be before <from>" >&2; exit 1; }
  t="$from"
  while awk -v a="$t" -v b="$to" 'BEGIN{exit !(a<=b)}'; do
    shot "$t"
    t="$(awk -v a="$t" -v s="$step" 'BEGIN{print a+s}')"
  done
else
  shot "$from"
fi
