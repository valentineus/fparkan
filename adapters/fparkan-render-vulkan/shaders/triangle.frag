#version 450

layout(location = 0) in vec3 in_color;
layout(location = 1) in vec2 in_uv;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D base_color;

void main() {
    out_color = texture(base_color, in_uv) * vec4(in_color, 1.0);
}
