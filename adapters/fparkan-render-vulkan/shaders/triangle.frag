#version 450

layout(location = 0) in vec3 in_color;
layout(location = 1) in vec2 in_uv;
layout(location = 2) in vec2 in_detail_uv;
layout(location = 3) in float in_overlay_alpha;
layout(location = 4) in float in_fog_factor;
layout(location = 5) in vec3 in_base_diffuse;
layout(location = 6) in vec3 in_base_specular;
layout(location = 7) in vec3 in_overlay_diffuse;
layout(location = 8) in vec3 in_overlay_specular;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D base_color;
layout(set = 0, binding = 1) uniform sampler2D base_detail;
layout(set = 0, binding = 2) uniform sampler2D overlay_color;
layout(set = 0, binding = 3) uniform sampler2D overlay_detail;

layout(std140, set = 0, binding = 4) uniform FrameConstants {
    mat4 clip_from_world;
    vec4 directional_direction[4];
    vec4 directional_rgb[4];
    vec4 directional_coefficients[4];
    vec4 point_position_range;
    vec4 point_rgb_active;
    vec4 point_coefficients_active;
    vec4 lighting_floor;
    vec4 fog_color;
    vec4 fog_distances;
    vec4 camera_position;
} frame;

layout(push_constant) uniform MaterialConstants {
    layout(offset = 0) float alpha_cutoff;
    layout(offset = 4) float diffuse_alpha;
    layout(offset = 8) float overlay_diffuse_alpha;
    layout(offset = 12) float directional_r;
    layout(offset = 16) float directional_g;
    layout(offset = 20) float directional_b;
    layout(offset = 24) float additive_r;
    layout(offset = 28) float additive_g;
    layout(offset = 32) float additive_b;
    layout(offset = 36) float overlay_directional_r;
    layout(offset = 40) float overlay_directional_g;
    layout(offset = 44) float overlay_directional_b;
    layout(offset = 48) float overlay_additive_r;
    layout(offset = 52) float overlay_additive_g;
    layout(offset = 56) float overlay_additive_b;
    layout(offset = 60) float base_page_x;
    layout(offset = 64) float base_page_y;
    layout(offset = 68) float base_page_w;
    layout(offset = 72) float base_page_h;
    layout(offset = 76) float detail_page_x;
    layout(offset = 80) float detail_page_y;
    layout(offset = 84) float detail_page_w;
    layout(offset = 88) float detail_page_h;
    layout(offset = 92) float overlay_page_x;
    layout(offset = 96) float overlay_page_y;
    layout(offset = 100) float overlay_page_w;
    layout(offset = 104) float overlay_page_h;
    layout(offset = 108) float overlay_detail_page_x;
    layout(offset = 112) float overlay_detail_page_y;
    layout(offset = 116) float overlay_detail_page_w;
    layout(offset = 120) float overlay_detail_page_h;
    layout(offset = 124) float material_mode;
} material;

void main() {
    vec2 base_uv = vec2(material.base_page_x, material.base_page_y)
        + in_uv * vec2(material.base_page_w, material.base_page_h);
    vec2 detail_uv = vec2(material.detail_page_x, material.detail_page_y)
        + in_detail_uv * vec2(material.detail_page_w, material.detail_page_h);
    vec2 overlay_uv = vec2(material.overlay_page_x, material.overlay_page_y)
        + in_uv * vec2(material.overlay_page_w, material.overlay_page_h);
    vec2 overlay_detail_uv = vec2(material.overlay_detail_page_x, material.overlay_detail_page_y)
        + in_detail_uv * vec2(material.overlay_detail_page_w, material.overlay_detail_page_h);
    vec4 base = texture(base_color, base_uv);
    vec4 detail = texture(base_detail, detail_uv);
    vec4 overlay = texture(overlay_color, overlay_uv);
    vec4 overlay_detail_sample = texture(overlay_detail, overlay_detail_uv);
    int mode = int(round(material.material_mode));
    int combiner_mode = mode & 3;

    // Native sky mode 4 combines nebula and stars as
    // mix(TEX0.rgb, TEX1.rgb, TEX1.a) * DIFFUSE.rgb and carries DIFFUSE.a
    // through as the pass alpha.
    if (combiner_mode == 2) {
        vec3 sky_rgb = mix(base.rgb, detail.rgb, clamp(detail.a, 0.0, 1.0));
        out_color = vec4(sky_rgb * in_color, clamp(in_overlay_alpha, 0.0, 1.0));
        if (out_color.a < material.alpha_cutoff) {
            discard;
        }
        return;
    }

    // Native terrain mode 3 combines the base texture and the lightmap
    // without the regular terrain 2x combiner or NdotL calculation.
    if (combiner_mode == 3) {
        vec3 lightmap_lighting = max(
            vec3(material.additive_r, material.additive_g, material.additive_b),
            frame.lighting_floor.rgb
        );
        vec3 vertex_rgb = base.rgb * detail.rgb * lightmap_lighting * in_color;
        vec3 final_rgb = mix(vertex_rgb, frame.fog_color.rgb, in_fog_factor);
        out_color = vec4(final_rgb, base.a * material.diffuse_alpha);
        if (out_color.a < material.alpha_cutoff) {
            discard;
        }
        return;
    }

    float overlay_alpha = clamp(
        in_overlay_alpha * material.overlay_diffuse_alpha,
        0.0,
        1.0
    );
    bool terrain_base_only = (mode & 16) != 0;
    vec3 base_rgb = terrain_base_only ? base.rgb : 2.0 * base.rgb * detail.rgb;
    vec3 overlay_rgb = 2.0 * overlay.rgb * overlay_detail_sample.rgb;
    vec3 base_lit_rgb = base_rgb * in_base_diffuse + in_base_specular;
    vec3 overlay_lit_rgb = overlay_rgb * in_overlay_diffuse + in_overlay_specular;
    vec3 lit_rgb = mix(base_lit_rgb, overlay_lit_rgb, overlay_alpha);
    vec3 vertex_rgb = lit_rgb * in_color;
    vec3 final_rgb = combiner_mode == 0
        ? mix(vertex_rgb, frame.fog_color.rgb, in_fog_factor)
        : vertex_rgb;
    float final_alpha = combiner_mode == 1
        ? base.a * material.diffuse_alpha * in_overlay_alpha
        : base.a * material.diffuse_alpha;
    out_color = vec4(final_rgb, final_alpha);
    if (out_color.a < material.alpha_cutoff) {
        discard;
    }
}
