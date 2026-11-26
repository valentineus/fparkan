//! Present the authored hemispheres and sound for a shield contact.

use super::{
    ambient_effect_mesh_primitive_indices, ambient_effect_mesh_primitive_layout,
    ambient_effect_pipeline_state, append_effect_material_animation, append_environment_range,
    apply_effect_range_tint, audio::GameAudio, environment_vertex_with_alpha,
    prepare_effect_material, write_ambient_effect_mesh_primitive, ActiveMaterialState,
    AmbientEffectGeometryUpdate, EnvironmentGpuRange, MaterialAnimationBinding,
    PreparedEffectMaterial, StaticPreviewScene, AMBIENT_EFFECT_NATIVE_CATEGORY,
};
use fparkan_assets::load_resource_entry_bytes_from_root;
use fparkan_fx::{
    ambient_sound::Op2SoundCommand, clock::FxInstanceClock, decode_fxid,
    fx_visual::evaluate_f_gener_ball_mesh, FxDocument, FxOpcode,
};
use fparkan_render_vulkan::VulkanStaticMesh;
use fparkan_runtime::{shield_effect_sources, Engine, ShieldImpact};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    sync::Arc,
};

const EFFECT_SLOTS: usize = 3;
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

struct Layer {
    command: usize,
    ranges: Vec<(EnvironmentGpuRange, usize)>,
}

#[derive(Default)]
struct Pulse {
    active: Option<(FxInstanceClock, [[f32; 4]; 4])>,
    layers: Vec<Layer>,
}

struct Shield {
    document: Arc<FxDocument>,
    sound: Option<Op2SoundCommand>,
    slots: [Pulse; EFFECT_SLOTS],
    next: usize,
}

#[derive(Default)]
pub(super) struct ShieldEffects {
    shields: BTreeMap<(usize, (String, String)), Shield>,
    ranges: HashMap<usize, EnvironmentGpuRange>,
}

impl ShieldEffects {
    pub(super) fn prepare(
        root: &Path,
        engine: &Engine,
        preview: &mut StaticPreviewScene,
    ) -> Result<Self, String> {
        let mut effects = Self::default();
        let mut documents: HashMap<_, Option<Arc<FxDocument>>> = HashMap::new();
        let mut materials: HashMap<String, PreparedEffectMaterial> = HashMap::new();
        for (index, source) in shield_effect_sources(engine).collect::<BTreeSet<_>>() {
            let (archive, name) = &source;
            let document = documents.entry(source.clone()).or_insert_with(|| {
                let bytes = load_resource_entry_bytes_from_root(root, archive, name).ok()?;
                decode_fxid(bytes).ok().map(Arc::new)
            });
            let Some(document) = document.as_ref() else {
                eprintln!("render_coverage shield_fx_stub=1 object={index} effect={archive}/{name} reason=document");
                continue;
            };
            let mut slots: [Pulse; EFFECT_SLOTS] = Default::default();
            for (command_index, command) in document.commands().iter().enumerate() {
                if command.opcode != FxOpcode::Op9 {
                    continue;
                }
                let prepared = command.resource_refs.first().and_then(|material| {
                    prepare_effect_material(
                        root,
                        std::str::from_utf8(material.archive_name()).ok()?,
                        std::str::from_utf8(material.resource_name()).ok()?,
                        &mut preview.materials,
                        &mut materials,
                    )
                    .ok()
                });
                let Some((material, first, pipeline)) = prepared.and_then(|material| {
                    let first = *material.phase_material_indices.first()?;
                    let pipeline =
                        ambient_effect_pipeline_state(material.initial_pipeline_category)?;
                    Some((material, first, pipeline))
                }) else {
                    eprintln!("render_coverage shield_fx_stub=1 object={index} effect={archive}/{name} reason=material");
                    continue;
                };
                let Some(template) = evaluate_f_gener_ball_mesh(
                    command,
                    command_index,
                    &IDENTITY,
                    0.5,
                    0.25,
                    [0.0; 3],
                    AMBIENT_EFFECT_NATIVE_CATEGORY,
                ) else {
                    eprintln!("render_coverage shield_fx_stub=1 object={index} effect={archive}/{name} reason=geometry");
                    continue;
                };
                let primitives = ambient_effect_mesh_primitive_layout(&template)?;
                for slot in &mut slots {
                    let mut ranges = Vec::with_capacity(primitives.len());
                    for &primitive in &primitives {
                        let vertices =
                            vec![
                                environment_vertex_with_alpha([0.0; 3], [1.0; 3], 0.0, [0.0; 2]);
                                primitive.vertex_count
                            ];
                        let range = append_environment_range(
                            &mut preview.mesh,
                            first,
                            vertices,
                            ambient_effect_mesh_primitive_indices(&template, primitive)?,
                            pipeline,
                        )?;
                        let binding = append_effect_material_animation(
                            &material,
                            range.range_index,
                            &mut preview.material_animations,
                        );
                        effects.ranges.insert(range.range_index, range);
                        ranges.push((range, binding));
                    }
                    slot.layers.push(Layer {
                        command: command_index,
                        ranges,
                    });
                }
            }
            effects.shields.insert(
                (index, source),
                Shield {
                    document: Arc::clone(document),
                    sound: document
                        .commands()
                        .iter()
                        .find_map(Op2SoundCommand::from_command),
                    slots,
                    next: 0,
                },
            );
        }
        Ok(effects)
    }

    pub(super) fn ranges(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges.keys().copied()
    }

    pub(super) fn tint(
        &self,
        mesh: &mut VulkanStaticMesh,
        updates: &[AmbientEffectGeometryUpdate],
        materials: &HashMap<usize, ActiveMaterialState>,
    ) -> Result<(), String> {
        for update in updates.iter().filter(|update| update.index_count > 0) {
            apply_effect_range_tint(mesh, &self.ranges[&update.range_index], update, materials)?;
        }
        Ok(())
    }

    pub(super) fn start(
        &mut self,
        impacts: Vec<ShieldImpact>,
        now_ms: u32,
        mut audio: Option<&mut GameAudio>,
    ) {
        for hit in impacts {
            let key = (hit.object_index, hit.source);
            let Some(shield) = self.shields.get_mut(&key) else {
                continue;
            };
            let header = shield.document.header();
            // Control+0x25ca0 starts mode 1 on one of three prebound instances.
            let Some(clock) =
                FxInstanceClock::new(1, header.flags, header.duration_seconds, now_ms as f32)
            else {
                continue;
            };
            shield.slots[shield.next].active = Some((clock, hit.world_from_effect));
            shield.next = (shield.next + 1) % EFFECT_SLOTS;
            if let Some(sound) = shield.sound.as_ref().and_then(|sound| {
                let center = std::array::from_fn(|axis| hit.world_from_effect[axis][3]);
                sound.one_shot_at(center)
            }) {
                if let Some(audio) = audio.as_deref_mut() {
                    if let Err(error) = audio.handle_sound_event(&sound) {
                        eprintln!(
                            "render_coverage shield_fx_stub=1 object={} reason={error}",
                            hit.object_index
                        );
                    }
                }
            }
        }
    }

    pub(super) fn sample(
        &mut self,
        now_ms: u32,
        camera: [f32; 3],
        mesh: &mut VulkanStaticMesh,
        materials: &mut [MaterialAnimationBinding],
    ) -> Result<Vec<AmbientEffectGeometryUpdate>, String> {
        let mut updates = Vec::new();
        for shield in self.shields.values_mut() {
            for slot in &mut shield.slots {
                let Some((mut clock, matrix)) = slot.active.take() else {
                    continue;
                };
                let now = now_ms as f32;
                let phase = clock
                    .update(now)
                    .and_then(|()| clock.phase(now, false, None));
                let elapsed = clock.elapsed_seconds(now).unwrap_or_default();
                if phase.is_some_and(|phase| phase < 1.0) {
                    slot.active = Some((clock, matrix));
                }
                for layer in &slot.layers {
                    let frame = phase
                        .filter(|&phase| phase < 1.0)
                        .and_then(|phase| {
                            evaluate_f_gener_ball_mesh(
                                &shield.document.commands()[layer.command],
                                layer.command,
                                &matrix,
                                phase,
                                elapsed,
                                camera,
                                AMBIENT_EFFECT_NATIVE_CATEGORY,
                            )
                        })
                        .filter(|frame| frame.active && frame.color[3] > 0.0);
                    let Some(frame) = frame else {
                        updates.extend(layer.ranges.iter().map(|(range, _)| {
                            AmbientEffectGeometryUpdate {
                                range_index: range.range_index,
                                index_count: 0,
                                command_rgb: [0.0; 3],
                            }
                        }));
                        continue;
                    };
                    let primitives = ambient_effect_mesh_primitive_layout(&frame)?;
                    if primitives.len() != layer.ranges.len() {
                        return Err("shield Op9 mesh changed its topology".into());
                    }
                    for (primitive, (range, material)) in primitives.into_iter().zip(&layer.ranges)
                    {
                        materials[*material].normalized_phase_override = Some(frame.material_phase);
                        write_ambient_effect_mesh_primitive(mesh, range, &frame, primitive)?;
                        updates.push(AmbientEffectGeometryUpdate {
                            range_index: range.range_index,
                            index_count: primitive.index_count as u32,
                            command_rgb: [frame.color[0], frame.color[1], frame.color[2]],
                        });
                    }
                }
            }
        }
        Ok(updates)
    }
}
