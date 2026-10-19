// egui's vertices in points; the Vulkan overlay renderer (src/win/vk). Built by build.sh.
#version 450

layout(push_constant) uniform Push {
    vec2 screen;        // the target's size in points
    float linear_target; // 1 when the target format is sRGB (stores linear values)
} pc;

layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
layout(location = 2) in vec4 a_color;

layout(location = 0) out vec2 v_uv;
layout(location = 1) out vec4 v_color;

void main() {
    // Vulkan's clip space has y pointing down, like egui's points.
    gl_Position = vec4(2.0 * a_pos / pc.screen - 1.0, 0.0, 1.0);
    v_uv = a_uv;
    v_color = a_color;
}
