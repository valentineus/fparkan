//! Present the attached explosion after the destroyed mesh is removed.

use super::{
    ambient_effect_pipeline_state, ambient_effect_vertex_rgb, append_effect_material_animation,
    append_environment_range, audio::GameAudio, debris::MeshFragment,
    environment_vertex_with_alpha, native_command_phase, prepare_effect_material,
    EnvironmentGpuRange, MaterialAnimationBinding, PreparedEffectMaterial, StaticPreviewScene,
};
use fparkan_assets::{decode_explosion_effect, load_resource_entry_bytes_from_root, TerrainWorld};
use fparkan_fx::{
    ambient_op3::evaluate_op3_type0_command, ambient_sound::Op2SoundCommand,
    clock::FxInstanceClock, decode_fxid, environment::evaluate_op1_point_light_with_basis,
    FxCommand, FxOpcode,
};
use fparkan_render_vulkan::{VulkanPointLight, VulkanStaticMesh};
use fparkan_runtime::{unit_destruction_effect_sources, Engine, UnitDestructionEffect};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

struct Explosion {
    duration: f32,
    flags: u32,
    basis: [[f32; 3]; 3],
    lights: Vec<FxCommand>,
    sound: Option<Op2SoundCommand>,
    sprite_pool: Option<usize>,
    fragments: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires installed GOG assets; reads the authored explosion curves"]
    fn stock_chassis_blast_fades_at_the_destroyed_position() {
        let root = std::env::var_os("FPARKAN_GAME_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        let mut effects = DestructionEffects::new(&root);
        let position = [23.0, 45.0, 10.0];
        effects.start(
            vec![UnitDestructionEffect {
                object_index: None,
                position,
                archive: "system.rlb".into(),
                name: "explode_rbr_b.exp".into(),
            }],
            100,
            None,
            |_, _| Vec::new(),
        );
        let mut lights = Vec::new();
        let mut mesh = VulkanStaticMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            draw_ranges: Vec::new(),
        };
        effects.sample(100, &mut lights, &mut mesh, &mut [], [0.0; 3]);
        assert_eq!(lights.len(), 1);
        assert_eq!(lights[0].position, position);
        assert_eq!(lights[0].range, 15.0);
        assert!(effects.active[0].effect.sound.is_some());
        assert!(effects.active[0].effect.fragments);
        lights.clear();
        effects.sample(1_300, &mut lights, &mut mesh, &mut [], [0.0; 3]);
        assert!((lights[0].range - 10.0).abs() < 0.0001);
        lights.clear();
        effects.sample(2_501, &mut lights, &mut mesh, &mut [], [0.0; 3]);
        assert!(lights.is_empty());
        effects.sample(12_100, &mut lights, &mut mesh, &mut [], [0.0; 3]);
        assert!(effects.active.is_empty());
    }
}

impl Explosion {
    fn load(root: &Path, archive: &str, name: &str) -> Option<(Self, Option<(usize, FxCommand)>)> {
        let bytes = load_resource_entry_bytes_from_root(root, archive, name).ok()?;
        let (archive, name) = decode_explosion_effect(&bytes)?;
        let fx =
            decode_fxid(load_resource_entry_bytes_from_root(root, archive, name).ok()?).ok()?;
        let header = fx.header();
        if header.time_mode != 1
            || !header.duration_seconds.is_finite()
            || header.duration_seconds <= 0.0
        {
            return None;
        }
        let [x, y, z] = header.scale;
        let basis = [[x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, z]];
        let lights: Vec<_> = fx
            .commands()
            .iter()
            .filter(|command| command.opcode == FxOpcode::Op1)
            .cloned()
            .collect();
        let sound = fx.commands().iter().find_map(Op2SoundCommand::from_command);
        let sprite = fx
            .commands()
            .iter()
            .enumerate()
            .find(|(_, command)| {
                command.opcode == FxOpcode::Op3 && command.raw_body.starts_with(&[0; 4])
            })
            .map(|(index, command)| (index, command.clone()));
        let fragments = fx.commands().iter().any(|command| {
            command.opcode == FxOpcode::Op10
                && command.resource_refs.first().is_some_and(|reference| {
                    reference.archive_name().is_empty() && reference.resource_name().is_empty()
                })
        });
        (!lights.is_empty() || sound.is_some() || sprite.is_some() || fragments).then_some((
            Self {
                duration: header.duration_seconds,
                flags: header.flags,
                basis,
                lights,
                sound,
                sprite_pool: None,
                fragments,
            },
            sprite,
        ))
    }
}

struct ActiveExplosion {
    effect: Arc<Explosion>,
    clock: FxInstanceClock,
    position: [f32; 3],
    fragments: Vec<MeshFragment>,
}

struct BlastSpritePool {
    command_index: usize,
    command: FxCommand,
    slots: Vec<BlastSpriteRange>,
}

struct BlastSpriteRange {
    range: EnvironmentGpuRange,
    material: usize,
    visible: bool,
}

pub(super) struct DestructionEffects {
    root: PathBuf,
    cache: HashMap<(String, String), Option<Arc<Explosion>>>,
    active: Vec<ActiveExplosion>,
    sprites: Vec<BlastSpritePool>,
    visible_fragments: HashSet<usize>,
}

impl DestructionEffects {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            cache: HashMap::new(),
            active: Vec::new(),
            sprites: Vec::new(),
            visible_fragments: HashSet::new(),
        }
    }

    pub(super) fn prepare(
        root: &Path,
        engine: &Engine,
        preview: &mut StaticPreviewScene,
    ) -> Result<Self, String> {
        let mut effects = Self::new(root);
        let mut owners = BTreeMap::new();
        for source in unit_destruction_effect_sources(engine) {
            *owners.entry(source).or_insert(0) += 1;
        }
        let mut materials = HashMap::<String, PreparedEffectMaterial>::new();
        for ((archive, name), count) in owners {
            if let Some((mut effect, sprite)) = Explosion::load(root, &archive, &name) {
                if let Some((command_index, command)) = sprite {
                    let prepared = command.resource_refs.first().and_then(|reference| {
                        prepare_effect_material(
                            root,
                            std::str::from_utf8(reference.archive_name()).ok()?,
                            std::str::from_utf8(reference.resource_name()).ok()?,
                            &mut preview.materials,
                            &mut materials,
                        )
                        .ok()
                    });
                    if let Some((material, selector, pipeline)) = prepared.and_then(|material| {
                        let selector = *material.phase_material_indices.first()?;
                        let pipeline =
                            ambient_effect_pipeline_state(material.initial_pipeline_category)?;
                        Some((material, selector, pipeline))
                    }) {
                        let slots = (0..count)
                            .map(|_| {
                                let range = append_environment_range(
                                    &mut preview.mesh,
                                    selector,
                                    vec![
                                        environment_vertex_with_alpha(
                                            [0.0; 3], [1.0; 3], 0.0, [0.0; 2]
                                        );
                                        4
                                    ],
                                    vec![0, 1, 2, 2, 3, 0],
                                    pipeline,
                                )?;
                                let material = append_effect_material_animation(
                                    &material,
                                    range.range_index,
                                    &mut preview.material_animations,
                                );
                                Ok::<_, String>(BlastSpriteRange {
                                    range,
                                    material,
                                    visible: false,
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        effect.sprite_pool = Some(effects.sprites.len());
                        effects.sprites.push(BlastSpritePool {
                            command_index,
                            command,
                            slots,
                        });
                    } else {
                        eprintln!("sim_coverage unit_destruction_effect_stub=1 effect={archive}/{name} reason=sprite_material");
                    }
                }
                effects
                    .cache
                    .insert((archive, name), Some(Arc::new(effect)));
            }
        }
        Ok(effects)
    }

    pub(super) fn start(
        &mut self,
        events: Vec<UnitDestructionEffect>,
        now: u32,
        mut audio: Option<&mut GameAudio>,
        mut capture_fragments: impl FnMut(usize, [f32; 3]) -> Vec<MeshFragment>,
    ) {
        for UnitDestructionEffect {
            object_index,
            position,
            archive,
            name,
        } in events
        {
            let effect = self
                .cache
                .entry((archive, name))
                .or_insert_with_key(|(archive, name)| {
                    let effect =
                        Explosion::load(&self.root, archive, name).map(|(effect, _)| effect);
                    if effect.is_none() {
                        eprintln!(
                            "sim_coverage unit_destruction_effect_stub=1 effect={archive}/{name}"
                        );
                    }
                    effect.map(Arc::new)
                });
            let Some(effect) = effect.as_ref() else {
                continue;
            };
            let Some(clock) = FxInstanceClock::new(1, effect.flags, effect.duration, now as f32)
            else {
                continue;
            };
            if let Some(sound) = effect
                .sound
                .as_ref()
                .and_then(|sound| sound.one_shot_at(position))
            {
                if let Some(audio) = audio.as_deref_mut() {
                    if let Err(error) = audio.handle_sound_event(&sound) {
                        eprintln!(
                            "destruction_sound resource={}:{} error={error}",
                            sound.archive, sound.name
                        );
                    }
                }
            }
            self.active.push(ActiveExplosion {
                effect: Arc::clone(effect),
                clock,
                position,
                fragments: object_index
                    .filter(|_| effect.fragments)
                    .map_or_else(Vec::new, |index| capture_fragments(index, position)),
            });
        }
    }

    pub(super) fn sample(
        &mut self,
        now: u32,
        lights: &mut Vec<VulkanPointLight>,
        mesh: &mut VulkanStaticMesh,
        materials: &mut [MaterialAnimationBinding],
        camera: [f32; 3],
    ) {
        let now = now as f32;
        for slot in self.sprites.iter_mut().flat_map(|pool| &mut pool.slots) {
            slot.visible = false;
        }
        self.active.retain_mut(|active| {
            let Some(elapsed) = active.clock.elapsed_seconds(now) else {
                return false;
            };
            if elapsed >= active.effect.duration {
                return false;
            }
            let Some(phase) = active.clock.phase(now, false, None) else {
                return false;
            };
            let phase = native_command_phase(active.effect.flags, phase, 0.0);
            lights.extend(active.effect.lights.iter().filter_map(|command| {
                let light = evaluate_op1_point_light_with_basis(
                    command,
                    phase,
                    active.position,
                    active.effect.basis,
                )?;
                light.active.then_some(VulkanPointLight {
                    owner_scope: 0,
                    position: light.position,
                    rgb: [light.color[0], light.color[1], light.color[2]],
                    range: light.range,
                    coefficients: light.attenuation,
                    active: true,
                })
            }));
            if let Some(pool) = active.effect.sprite_pool {
                self.sprites[pool].sample(active, elapsed, phase, camera, mesh, materials);
            }
            true
        });
    }

    pub(super) fn draw_fragments(
        &mut self,
        now: u32,
        mesh: &mut VulkanStaticMesh,
        terrain: &TerrainWorld,
    ) {
        self.visible_fragments.clear();
        for active in &self.active {
            if let Some(elapsed) = active.clock.elapsed_seconds(now as f32) {
                for fragment in &active.fragments {
                    fragment.draw(elapsed, mesh, terrain);
                    self.visible_fragments.extend(&fragment.ranges);
                }
            }
        }
    }

    pub(super) fn keeps_range(&self, index: usize) -> bool {
        self.visible_fragments.contains(&index)
    }

    pub(super) fn inactive_ranges(&self) -> impl Iterator<Item = usize> + '_ {
        self.sprites
            .iter()
            .flat_map(|pool| &pool.slots)
            .filter(|slot| !slot.visible)
            .map(|slot| slot.range.range_index)
    }
}

impl BlastSpritePool {
    fn sample(
        &mut self,
        active: &ActiveExplosion,
        elapsed: f32,
        phase: f32,
        camera: [f32; 3],
        mesh: &mut VulkanStaticMesh,
        materials: &mut [MaterialAnimationBinding],
    ) {
        let [x, y, z] = active.position;
        let basis = active.effect.basis;
        let matrix = [
            [basis[0][0], 0.0, 0.0, x],
            [0.0, basis[1][1], 0.0, y],
            [0.0, 0.0, basis[2][2], z],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let Some(frame) = evaluate_op3_type0_command(
            &self.command,
            self.command_index,
            &matrix,
            elapsed,
            phase,
            camera,
        ) else {
            return;
        };
        if !frame.active || frame.color[3] <= 0.0 {
            return;
        }
        let Some(slot) = self.slots.iter_mut().find(|slot| !slot.visible) else {
            return;
        };
        let binding = &mut materials[slot.material];
        let Ok(material) = binding
            .base
            .sample_normalized(binding.initial_phase_index, frame.material_phase)
        else {
            return;
        };
        binding.normalized_phase_override = Some(frame.material_phase);
        let [r, g, b, alpha] = frame.color;
        let rgb = ambient_effect_vertex_rgb([r, g, b], material.coefficients.additive_rgb);
        for (i, vertex) in mesh.vertices[slot.range.vertex_start..][..4]
            .iter_mut()
            .enumerate()
        {
            *vertex = environment_vertex_with_alpha(frame.vertices[i], rgb, alpha, frame.uv[i]);
        }
        slot.visible = true;
    }
}
