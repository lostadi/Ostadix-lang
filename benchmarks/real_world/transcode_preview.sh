#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
    printf 'usage: %s INPUT OUTPUT LABEL\n' "$0" >&2
    exit 2
fi

input=$1
output=$2
label=$3
duration=${OSTADIX_PREVIEW_SECONDS:-3}
case "$duration" in
    ''|*[!0-9]*)
        printf 'OSTADIX_PREVIEW_SECONDS must be an integer from 1 through 30\n' >&2
        exit 2
        ;;
esac
if [[ "$duration" -lt 1 || "$duration" -gt 30 ]]; then
    printf 'OSTADIX_PREVIEW_SECONDS must be an integer from 1 through 30\n' >&2
    exit 2
fi

mkdir -p -- "$(dirname -- "$output")"
ffmpeg -nostdin -hide_banner -loglevel error \
    -stream_loop -1 -i "$input" -t "$duration" \
    -vf 'fps=30,scale=768:832:flags=lanczos,format=yuv420p' \
    -an -c:v libvpx-vp9 -deadline good -cpu-used 5 \
    -threads 1 -row-mt 0 -crf 32 -b:v 0 \
    -map_metadata -1 -y "$output"

printf 'preview=%s codec=vp9 size=768x832 status=encoded\n' "$label"
