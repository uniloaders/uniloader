#!/usr/bin/env bash
# Compile the Vulkan overlay's shaders to the SPIR-V the plugin embeds
# (src/win/vk/*.spv). Run after editing a shader; the outputs are committed, so a normal
# build needs no shader compiler. Needs glslc (the build-box has it).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/../src/win/vk"
for stage in vert frag; do
    glslc -O --target-env=vulkan1.0 -o "$out/egui.$stage.spv" "$here/egui.$stage"
done
echo "shaders written to $out"
