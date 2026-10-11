#version 450

layout(location = 0) in vec3 in_position;
layout(location = 1) in vec3 in_color;
layout(location = 2) in vec3 in_normal;
layout(location = 3) in vec2 in_uv;
layout(location = 4) in vec2 in_detail_uv;
layout(location = 5) in float in_overlay_alpha;

layout(location = 0) out vec3 out_color;
layout(location = 1) out vec2 out_uv;
layout(location = 2) out vec2 out_detail_uv;
layout(location = 3) out float out_overlay_alpha;
// Native D3D vertex fog is written to SPECULAR.a and interpolated by the
// fixed-function rasterizer.  Keep the existing varying slot, but carry only
// that scalar instead of recomputing distance in the fragment stage.
layout(location = 4) out float out_fog_factor;
layout(location = 5) out vec3 out_base_diffuse;
layout(location = 6) out vec3 out_base_specular;
layout(location = 7) out vec3 out_overlay_diffuse;
layout(location = 8) out vec3 out_overlay_specular;

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

layout(std430, set = 0, binding = 5) readonly buffer MaterialSpecular {
    vec4 base;
    vec4 overlay;
} material_specular;

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

struct LightingTerms {
    vec3 diffuse;
    vec3 specular;
};

float native_range_compress(float value) {
    if (value <= 1.0) {
        return value;
    }
    if (value <= 7.0) {
        return value / 6.0 + 5.0 / 6.0;
    }
    return 2.0;
}

float native_specular_curve(float value) {
    if (value <= 1.0) {
        return 0.8 * value;
    }
    if (value <= 3.0) {
        return 0.1 * value + 0.7;
    }
    return 1.0;
}

// The original callback uses the reflected vector and squares the cosine for
// power-1 iterations.  A bounded loop keeps the native byte power while
// remaining valid for every material record.
float native_specular_power(float cosine, float power) {
    // CShade enables specular only when the material power byte is non-zero.
    if (power <= 0.0 || cosine <= 0.0) {
        return 0.0;
    }
    float result = cosine;
    int iterations = clamp(int(power) - 1, 0, 254);
    for (int index = 0; index < 254; ++index) {
        if (index >= iterations) {
            break;
        }
        result *= result;
    }
    return result;
}

float native_fog_factor(vec3 world_position) {
    float fog_span = frame.fog_distances.y - frame.fog_distances.x;
    float distance_to_camera = distance(world_position, frame.camera_position.xyz);
    if (fog_span <= 0.000001 || distance_to_camera <= frame.fog_distances.x) {
        return 0.0;
    }
    return clamp((distance_to_camera - frame.fog_distances.x) / fog_span, 0.0, 1.0);
}

vec3 normalized_or(vec3 value, vec3 fallback) {
    float length_squared = dot(value, value);
    return length_squared > 0.0000001 ? value * inversesqrt(length_squared) : fallback;
}

vec3 specular_from_light(
    vec3 normal,
    vec3 view_direction,
    vec3 light_direction,
    vec3 light_rgb,
    vec3 material_rgb,
    float power,
    float attenuation
) {
    float ndotl = dot(normal, light_direction);
    if (ndotl <= 0.0 || attenuation <= 0.0) {
        return vec3(0.0);
    }
    vec3 reflection = reflect(-light_direction, normal);
    float cosine = max(dot(reflection, view_direction), 0.0);
    return material_rgb * light_rgb * native_specular_power(cosine, power) * attenuation;
}

LightingTerms native_material_lighting(
    vec3 world_position,
    vec3 normal,
    vec3 directional_rgb,
    vec3 additive_rgb,
    vec3 specular_rgb,
    float specular_power
) {
    vec3 accumulated = additive_rgb;
    vec3 specular = vec3(0.0);
    vec3 view_direction = normalized_or(frame.camera_position.xyz - world_position, vec3(0.0, 0.0, 1.0));
    for (int index = 0; index < 4; ++index) {
        vec4 direction_record = frame.directional_direction[index];
        vec4 rgb_record = frame.directional_rgb[index];
        if (direction_record.w <= 0.5 || rgb_record.w <= 0.5) {
            continue;
        }
        vec3 light_direction = normalized_or(-direction_record.xyz, vec3(0.0, 0.0, 1.0));
        float ndotl = max(dot(normal, light_direction), 0.0);
        accumulated += ndotl * directional_rgb * rgb_record.rgb;
        specular += specular_from_light(
            normal,
            view_direction,
            light_direction,
            rgb_record.rgb,
            specular_rgb,
            specular_power,
            1.0
        );
    }
    vec4 point_position_range = frame.point_position_range;
    vec4 point_rgb_active = frame.point_rgb_active;
    vec4 point_coefficients_active = frame.point_coefficients_active;
    vec3 to_light = point_position_range.xyz - world_position;
    float distance_squared = dot(to_light, to_light);
    float range = point_position_range.w;
    if (point_rgb_active.w > 0.5 && range > 0.0 && distance_squared <= range * range) {
        float distance_to_light = sqrt(distance_squared);
        if (distance_to_light > 0.000001) {
            vec3 light_direction = to_light / distance_to_light;
            float ndotl = dot(normal, light_direction);
            if (ndotl > 0.0) {
                float t = (range - distance_to_light) / range;
                float attenuation = point_coefficients_active.x
                    + point_coefficients_active.y * t
                    + point_coefficients_active.z * t * t;
                attenuation = max(attenuation, 0.0);
                accumulated += ndotl * directional_rgb * point_rgb_active.rgb * attenuation;
                specular += specular_from_light(
                    normal,
                    view_direction,
                    light_direction,
                    point_rgb_active.rgb,
                    specular_rgb,
                    specular_power,
                    attenuation
                );
            }
        }
    }
    accumulated = max(accumulated, frame.lighting_floor.rgb);
    vec3 compressed = vec3(
        native_range_compress(accumulated.r),
        native_range_compress(accumulated.g),
        native_range_compress(accumulated.b)
    );
    LightingTerms terms;
    terms.diffuse = min(compressed, vec3(1.0));
    vec3 total_specular = specular + max(compressed - vec3(1.0), vec3(0.0));
    terms.specular = vec3(
        native_specular_curve(total_specular.r),
        native_specular_curve(total_specular.g),
        native_specular_curve(total_specular.b)
    );
    return terms;
}

void main() {
    out_color = in_color;
    out_uv = in_uv;
    out_detail_uv = in_detail_uv;
    out_overlay_alpha = in_overlay_alpha;
    out_fog_factor = native_fog_factor(in_position);
    int mode = int(round(material.material_mode));
    int combiner_mode = mode & 3;
    if (combiner_mode == 0) {
        LightingTerms base_terms = native_material_lighting(
            in_position,
            in_normal,
            vec3(material.directional_r, material.directional_g, material.directional_b),
            vec3(material.additive_r, material.additive_g, material.additive_b),
            material_specular.base.rgb,
            material_specular.base.w
        );
        LightingTerms overlay_terms = native_material_lighting(
            in_position,
            in_normal,
            vec3(material.overlay_directional_r, material.overlay_directional_g, material.overlay_directional_b),
            vec3(material.overlay_additive_r, material.overlay_additive_g, material.overlay_additive_b),
            material_specular.overlay.rgb,
            material_specular.overlay.w
        );
        out_base_diffuse = base_terms.diffuse;
        out_base_specular = base_terms.specular;
        out_overlay_diffuse = overlay_terms.diffuse;
        out_overlay_specular = overlay_terms.specular;
    } else {
        out_base_diffuse = vec3(1.0);
        out_base_specular = vec3(0.0);
        out_overlay_diffuse = vec3(1.0);
        out_overlay_specular = vec3(0.0);
    }
    vec4 clip_position = frame.clip_from_world * vec4(in_position, 1.0);
    // Sky far-depth is a Vulkan projection choice for layers authored around
    // the camera. Keep world positions, normals, and UVs intact while placing
    // the resulting primitive at the far depth; this does not assert native
    // D3D depth-bit equivalence.
    if ((mode & 4) != 0) {
        clip_position.z = clip_position.w;
    }
    gl_Position = clip_position;
}
