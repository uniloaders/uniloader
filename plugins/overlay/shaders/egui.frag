// egui's premultiplied sRGB colours; the Vulkan overlay renderer (src/win/vk). Built by build.sh.
#version 450

layout(set = 0, binding = 0) uniform sampler2D tex;

layout(push_constant) uniform Push {
    vec2 screen;
    float linear_target;
} pc;

layout(location = 0) in vec2 v_uv;
layout(location = 1) in vec4 v_color;

layout(location = 0) out vec4 frag;

vec3 linear_from_gamma(vec3 c) {
    bvec3 low = lessThan(c, vec3(0.04045));
    return mix(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, low);
}

void main() {
    vec4 c = v_color * texture(tex, v_uv);
    // An sRGB target encodes what it is given: hand it linear values, or egui comes out pale.
    if (pc.linear_target > 0.5) {
        c.rgb = linear_from_gamma(c.rgb);
    }
    frag = c;
}
