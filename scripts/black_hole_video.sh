#!/usr/bin/env bash
# Renders the black_hole_cinematic example to out/black_hole_cinematic.mp4:
# 1920x1080, 60 fps, 40 s. Captures every tick at 3840x2160 through a
# headless scenario and scales down to 1080p, so the photon ring and the
# lensed stars come out supersampled instead of flickering.
set -euo pipefail
cd "$(dirname "$0")/.."

frames=2400
mkdir -p out
rm -rf out/bh_frames
mkdir out/bh_frames

steps=$(for ((i = 0; i < frames; i++)); do
    printf '{"tick":%d,"capture":{"path":"bh_frames/%05d.png","hud":false}},' "$i" "$i"
done)
printf '{"name":"black hole video","ticks":%d,"capture_size":[3840,2160],"steps":[%s]}\n' \
    "$frames" "${steps%,}" > out/black_hole.scenario.json

cargo build --release --features window --example black_hole_cinematic
RUSTING_TEST_SCENARIO=out/black_hole.scenario.json RUSTING_TEST_STALL_SECS=0 \
    target/release/examples/black_hole_cinematic

ffmpeg -nostdin -y -loglevel error -framerate 60 -i out/bh_frames/%05d.png \
    -vf scale=1920:1080:flags=lanczos \
    -c:v libx264 -preset slow -crf 14 -pix_fmt yuv420p -movflags +faststart \
    out/black_hole_cinematic.mp4
rm -rf out/bh_frames out/black_hole.scenario.json
echo "wrote out/black_hole_cinematic.mp4"
