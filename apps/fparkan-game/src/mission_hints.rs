use super::{
    append_environment_gradient_material, append_environment_range, audio, environment_vertex,
    quad_indices, screen_tl_quad_vertices_pixels, world_pipeline_state, DirectoryVfs,
    EnvironmentGpuRange, LegacyBlendMode, LegacyDepthMode, StaticPreviewScene, Vfs,
    VulkanSmokeRenderer, VulkanStaticMesh, VulkanStaticTexture,
};
use fparkan_path::{normalize_relative, PathPolicy};
use fparkan_platform_winit::text::{caption, StringResources};
use fparkan_vfs::VfsError;
use std::{collections::VecDeque, path::Path};

const WIDTH: u16 = 640;
const HEIGHT: u16 = 384;
const UV: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];

pub(super) struct MissionHints {
    config: std::sync::Arc<[u8]>,
    catalog: std::sync::Arc<[u8]>,
    strings: StringResources,
    range: EnvironmentGpuRange,
    waiting: VecDeque<(u32, String)>,
    active: Option<(u32, u32, u32)>,
}

impl MissionHints {
    pub(super) fn range_index(&self) -> usize {
        self.range.range_index
    }

    pub(super) fn load(
        root: &Path,
        mission: &str,
        preview: &mut StaticPreviewScene,
    ) -> Result<Option<Self>, String> {
        let vfs = DirectoryVfs::new(root);
        let path = mission.rsplit_once('/').map_or_else(
            || "messages.cfg".into(),
            |(dir, _)| format!("{dir}/messages.cfg"),
        );
        let config = match vfs.read(
            &normalize_relative(path.as_bytes(), PathPolicy::StrictLegacy)
                .map_err(|error| error.to_string())?,
        ) {
            Ok(bytes) => bytes,
            Err(VfsError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let catalog_path =
            normalize_relative(b"DATA/TextRes.cfg", PathPolicy::StrictLegacy).unwrap();
        let catalog = vfs.read(&catalog_path).map_err(|error| error.to_string())?;
        let text = String::from_utf8_lossy(&catalog);
        let library = audio::config_objects(&text)
            .find_map(|object| object.get("LIBRARY").copied())
            .ok_or_else(|| "missing text resource library".to_owned())?;
        let library = audio::parse_cfg_value(library)?;
        let library = normalize_relative(library.as_bytes(), PathPolicy::StrictLegacy)
            .map_err(|error| error.to_string())?;
        let strings = StringResources::open(&root.join(library.as_str()))?;
        let material = append_environment_gradient_material(&mut preview.materials)?;
        preview.materials[material].screen_tl = true;
        preview.materials[material].sky_far_depth = false;
        preview.materials[material].texture =
            texture(vec![0; usize::from(WIDTH) * usize::from(HEIGHT) * 4]);
        let range = append_environment_range(
            &mut preview.mesh,
            material,
            vec![environment_vertex([0.0; 3], [1.0; 3], [0.0; 2]); 4],
            quad_indices(1)?,
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::Disabled),
        )?;
        eprintln!("sim_coverage mission_text_display_approx=1");
        // Keep the degenerate reservation until the renderer receives live caption vertices.
        Ok(Some(Self {
            config,
            catalog,
            strings,
            range,
            waiting: VecDeque::new(),
            active: None,
        }))
    }

    pub(super) fn enqueue(&mut self, index: u32) -> Result<(), String> {
        let symbol = audio::message_symbol(&self.config, index, "TEXT_RESOURCE")?
            .ok_or_else(|| format!("missing TEXT_RESOURCE for message {index}"))?;
        let symbol = symbol.to_ascii_uppercase();
        let text = String::from_utf8_lossy(&self.catalog);
        let id = audio::config_objects(&text)
            .find_map(|object| object.get(&symbol).and_then(|value| value.parse().ok()))
            .ok_or_else(|| format!("missing string ID for {symbol}"))?;
        let body = self
            .strings
            .get(id)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| format!("missing string {id} for {symbol}"))?;
        self.waiting.push_back((index, body));
        Ok(())
    }

    pub(super) fn draw(
        &mut self,
        renderer: &mut VulkanSmokeRenderer,
        mesh: &mut VulkanStaticMesh,
        clock: u32,
        viewport: [f32; 2],
    ) -> Result<(), String> {
        if self
            .active
            .is_some_and(|(start, duration, _)| clock.wrapping_sub(start) >= duration)
        {
            self.active = None;
        }
        if self.active.is_none() {
            if let Some((index, body)) = self.waiting.pop_front() {
                let (height, rgba) = match caption(&body, WIDTH, HEIGHT) {
                    Ok(caption) => caption,
                    Err(error) => {
                        eprintln!(
                            "sim_coverage mission_text_unavailable=1 index={index} reason={error}"
                        );
                        return renderer
                            .set_draw_range_index_count(self.range.range_index, 0)
                            .map_err(|error| error.to_string());
                    }
                };
                renderer
                    .update_material_texture(self.range.material_index, 0, &texture(rgba))
                    .map_err(|error| error.to_string())?;
                // shortcut: reading time from character count until pager timing is recovered.
                let duration = (body.chars().count() as u32)
                    .saturating_mul(45)
                    .clamp(3500, 18_000);
                self.active = Some((clock, duration, height));
                eprintln!(
                    "mission_text index={index} queue_time_ms={clock} duration_ms={duration}"
                );
            }
        }
        if let Some((_, _, occupied)) = self.active {
            let scale = ((viewport[0] - 32.0) / f32::from(WIDTH)).clamp(0.01, 1.0);
            let [width, height] = [f32::from(WIDTH) * scale, occupied as f32 * scale];
            let mut uv = UV;
            uv[1][1] = occupied as f32 / f32::from(HEIGHT);
            uv[2][1] = uv[1][1];
            let vertices = screen_tl_quad_vertices_pixels(
                [viewport[0] * 0.5, 16.0 + height * 0.5],
                width * 0.5,
                height * 0.5,
                viewport,
                [1.0; 3],
                1.0,
                uv,
            );
            mesh.vertices[self.range.vertex_start..self.range.vertex_start + vertices.len()]
                .copy_from_slice(&vertices);
        }
        renderer
            .set_draw_range_index_count(
                self.range.range_index,
                if self.active.is_some() { 6 } else { 0 },
            )
            .map_err(|error| error.to_string())
    }
}

fn texture(rgba8: Vec<u8>) -> VulkanStaticTexture {
    VulkanStaticTexture {
        width: u32::from(WIDTH),
        height: u32::from(HEIGHT),
        rgba8,
        mip_levels: Vec::new(),
    }
}
