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
layout(location = 9) in vec3 in_world_position;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D base_color;
layout(set = 0, binding = 1) uniform sampler2D base_detail;
layout(set = 0, binding = 2) uniform sampler2D overlay_color;
layout(set = 0, binding = 3) uniform sampler2D overlay_detail;
layout(set = 1, binding = 0) uniform sampler2D reflection_color;
layout(set = 1, binding = 1) uniform sampler2D embm_bump;

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
    vec4 embm_bounds;
    vec4 embm_wave;
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

vec3 native_reflection_texel(ivec2 pixel, int mode) {
    ivec2 last = textureSize(reflection_color, 0) - ivec2(1);
    uvec3 rgb8 = uvec3(round(texelFetch(reflection_color, clamp(pixel, ivec2(0), last), 0).rgb * 255.0));
    bool rgb555 = (mode & 1024) != 0;
    uvec3 shift = rgb555 ? uvec3(3) : uvec3(3, 2, 3);
    vec3 maximum = rgb555 ? vec3(31.0) : vec3(31.0, 63.0, 31.0);
    return vec3(rgb8 >> shift) / maximum;
}

vec3 native_reflection_sample(vec2 uv, int mode) {
    if ((mode & 1536) == 0) {
        return texture(reflection_color, uv).rgb;
    }
    // GDI drops bits after all RGB8 blending, before the water sampler filters texels.
    vec2 point = uv * vec2(textureSize(reflection_color, 0)) - vec2(0.5);
    ivec2 pixel = ivec2(floor(point));
    vec2 weight = fract(point);
    return mix(
        mix(native_reflection_texel(pixel, mode), native_reflection_texel(pixel + ivec2(1, 0), mode), weight.x),
        mix(native_reflection_texel(pixel + ivec2(0, 1), mode), native_reflection_texel(pixel + ivec2(1), mode), weight.x),
        weight.y
    );
}

void main() {
    int mode = int(round(material.material_mode));
    if ((mode & 32) != 0 && (mode & 4) == 0 && in_world_position.z < frame.camera_position.w) {
        discard;
    }
    vec2 base_uv = vec2(material.base_page_x, material.base_page_y)
        + in_uv * vec2(material.base_page_w, material.base_page_h);
    if ((mode & (64 | 256)) != 0) {
        vec2 reflection_uv = gl_FragCoord.xy / vec2(textureSize(reflection_color, 0));
        vec2 span = frame.embm_bounds.zw - frame.embm_bounds.xy;
        vec2 plane_uv = vec2(1.0) - (in_world_position.yx - frame.embm_bounds.yx) / span.yx;
        vec2 bump_uv = plane_uv * frame.embm_wave.y + frame.embm_wave.x;
        vec2 duv = max(vec2(-1.0), (texture(embm_bump, bump_uv).rg * 255.0 - 128.0) / 127.0);
        vec2 displacement = duv * frame.embm_wave.zw;
        if ((mode & 128) != 0) {
            reflection_uv = plane_uv + displacement;
        } else {
            // shortcut: non-perspective API cameras retain the projective mirror fallback.
            vec3 displaced_world = in_world_position - vec3(displacement.yx * span, 0.0);
            vec4 displaced_clip = frame.clip_from_world * vec4(displaced_world, 1.0);
            if (displaced_clip.w > 0.000001) {
                reflection_uv = displaced_clip.xy / displaced_clip.w * 0.5 + 0.5;
            }
        }
        vec3 water_color = (mode & 256) != 0
            ? texture(base_color, base_uv).rgb
            : native_reflection_sample(reflection_uv, mode);
        vec3 rgb = water_color * in_base_diffuse + in_base_specular;
        out_color = vec4(mix(rgb * in_color, frame.fog_color.rgb, in_fog_factor), material.diffuse_alpha);
        if (out_color.a < material.alpha_cutoff) {
            discard;
        }
        return;
    }
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
    // Fixed-function color saturates before fog; framebuffer clamping is too late.
    vec3 final_rgb = combiner_mode == 0 || (mode & 512) != 0
        ? mix(clamp(vertex_rgb, vec3(0.0), vec3(1.0)), frame.fog_color.rgb, in_fog_factor)
        : vertex_rgb;
    float final_alpha = combiner_mode == 1
        ? base.a * material.diffuse_alpha * in_overlay_alpha
        : base.a * material.diffuse_alpha;
    out_color = vec4(final_rgb, final_alpha);
    if (out_color.a < material.alpha_cutoff) {
        discard;
    }
}
