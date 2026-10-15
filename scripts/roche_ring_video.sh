#!/usr/bin/env bash
# Renders the roche_ring example to out/roche_ring.mp4: 1080x1920, 60 fps,
# 32 s. Simulates first (about 12 minutes for 80,000 rocks), then captures
# one PNG a tick through a headless scenario and joins them with ffmpeg.
# Extra arguments go to the example, e.g. `--rocks 20000` for a quick draft.
set -euo pipefail
cd "$(dirname "$0")/.."

frames=1920
mkdir -p out
rm -rf out/roche_frames
mkdir out/roche_frames

steps=$(for ((i = 0; i < frames; i++)); do
    printf '{"tick":%d,"capture":{"path":"roche_frames/%05d.png","hud":false}},' "$i" "$i"
done)
printf '{"name":"roche ring video","ticks":%d,"capture_size":[1080,1920],"steps":[%s]}\n' \
    "$frames" "${steps%,}" > out/roche_ring.scenario.json

cargo build --release --features window --example roche_ring
RUSTING_TEST_SCENARIO=out/roche_ring.scenario.json RUSTING_TEST_STALL_SECS=0 \
    target/release/examples/roche_ring --portrait "$@"

ffmpeg -y -loglevel error -framerate 60 -i out/roche_frames/%05d.png \
    -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p -movflags +faststart \
    out/roche_ring.mp4
rm -rf out/roche_frames out/roche_ring.scenario.json
echo "wrote out/roche_ring.mp4"
