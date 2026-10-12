#!/usr/bin/env bash
# Compiles every shader file under src/shaders with glslc, without building
# the crate, so a broken shader fails fast with glslc's own message. Shaders
# written inline in Rust (`src: r"..."`) compile only through `cargo build`.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$script_dir/.."

failed=0
count=0
for shader in src/shaders/vertex/*.vert src/shaders/fragment/*.frag \
    src/shaders/compute/*.comp; do
    count=$((count + 1))
    if ! glslc --target-env=vulkan1.2 -I src/shaders -o /dev/null "$shader"; then
        failed=$((failed + 1))
    fi
done
echo "shaders: $count checked, $failed failed"
[ "$failed" -eq 0 ]
