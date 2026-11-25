use super::UnitPatrol;
use crate::{MissionDocument, MissionObjectDraft};
use fparkan_script::{Handler15Invocation, Handler15TargetPayload};
use fparkan_world::{identity_metadata, transform_state, ObjectHandle, World};
use std::collections::VecDeque;

#[derive(Clone, Copy)]
pub(super) enum UnitOrder {
    Move([f32; 2]),
    Attack(ObjectHandle, usize),
    Patrol([f32; 2], f32),
    Shutdown,
}

impl UnitOrder {
    fn opcode(self) -> u32 {
        match self {
            Self::Move(_) => 2,
            Self::Attack(..) => 3,
            Self::Patrol(..) => 4,
            Self::Shutdown => 0x13,
        }
    }
}

pub(crate) fn supports_order(order: &Handler15Invocation) -> bool {
    if matches!(
        (order.word_1, order.word_2, order.target),
        (4, 1, Handler15TargetPayload::ByPlace { .. })
    ) {
        return true;
    }
    matches!(order.word_2, 1 | 3)
        && matches!(
            (order.word_1, order.target),
            (2, Handler15TargetPayload::ByPlace { .. })
                | (3, Handler15TargetPayload::ByLogicId(_))
                | (0x13, Handler15TargetPayload::None)
        )
}

impl UnitPatrol {
    pub(crate) fn script_order(
        &mut self,
        world: &World,
        mission: &MissionDocument,
        drafts: &[MissionObjectDraft],
        order: Handler15Invocation,
    ) -> Option<u32> {
        if !supports_order(&order) {
            return None;
        }
        let Some(actor) =
            crate::script_patrol::unit_by_logic_id(mission, drafts, world, order.word_0, |class| {
                class & crate::autodemo::MOBILE_UNIT != 0 && class != crate::HERO_UNIT_CLASS
            })
        else {
            return Some(5);
        };
        let command = match order.target {
            Handler15TargetPayload::ByPlace { first, second } => {
                let point = [first as f32, second as f32];
                if order.word_1 == 4 {
                    let radius = match order.word_3 {
                        0 | u32::MAX => 60.0,
                        value => value as f32,
                    };
                    UnitOrder::Patrol(point, radius)
                } else {
                    UnitOrder::Move(point)
                }
            }
            Handler15TargetPayload::ByLogicId(id) => {
                let Some(target) =
                    crate::mission_events::placed_logic_objects(mission, world, id).next()
                else {
                    return Some(0);
                };
                let Some(index) = identity_metadata(world, target)
                    .ok()
                    .and_then(|meta| meta.original_id)
                else {
                    return Some(0);
                };
                UnitOrder::Attack(target, index.0 as usize)
            }
            Handler15TargetPayload::None => UnitOrder::Shutdown,
            _ => unreachable!("supported target form checked above"),
        };
        self.enqueue_script_order(world, actor, command, order.word_2 == 1);
        Some(1)
    }

    fn enqueue_script_order(
        &mut self,
        world: &World,
        actor: ObjectHandle,
        order: UnitOrder,
        append: bool,
    ) {
        let count = self
            .script_order_counts
            .entry((actor.slot, order.opcode()))
            .or_default();
        *count += 1;
        if *count == 1 {
            eprintln!(
                "sim_coverage script_unit_queue_approx=1 unit={} order={} append={append}",
                actor.slot,
                order.opcode()
            );
        }
        if append && (self.is_patrolling(actor) || self.is_shut_down(actor)) {
            if let Some((_, queue)) = self
                .script_orders
                .iter_mut()
                .find(|(unit, _)| *unit == actor)
            {
                queue.push_back(order);
            } else {
                self.script_orders.push((actor, VecDeque::from([order])));
            }
            return;
        }
        self.script_orders.retain(|(unit, _)| *unit != actor);
        self.start_script_order(world, actor, order);
    }

    fn start_script_order(&mut self, world: &World, actor: ObjectHandle, order: UnitOrder) -> bool {
        match order {
            UnitOrder::Move(point) => {
                self.move_to(world, actor, point, false);
                eprintln!(
                    "sim_coverage script_move_approx=1 unit={} destination={point:?}",
                    actor.slot
                );
            }
            UnitOrder::Attack(target, index) => {
                let Ok(pose) = transform_state(world, target) else {
                    return false;
                };
                let [x, y, _] = pose.position.map(f32::from_bits);
                self.set_attack_target(world, actor, target, index, [x, y]);
                eprintln!(
                    "sim_coverage script_attack_approx=1 unit={} target_unit={}",
                    actor.slot, target.slot
                );
            }
            UnitOrder::Shutdown => {
                self.shut_down(actor);
                eprintln!("sim_coverage unit_shutdown_approx=1 unit={}", actor.slot);
            }
            UnitOrder::Patrol(point, radius) => {
                self.order(world, actor, point, radius);
                eprintln!("sim_coverage script_point_patrol_approx=1 unit={} center={point:?} radius={radius}", actor.slot);
            }
        }
        eprintln!(
            "script_unit_order_started unit={} order={}",
            actor.slot,
            order.opcode()
        );
        true
    }

    pub(super) fn advance_script_orders(&mut self, world: &World) {
        for (actor, mut queue) in std::mem::take(&mut self.script_orders) {
            let Ok(pose) = transform_state(world, actor) else {
                continue;
            };
            // shortcut: use current Go arrival and target removal until native task completion is wired.
            let complete = !self.is_shut_down(actor)
                && self
                    .units
                    .iter()
                    .find(|unit| unit.handle == actor)
                    .is_none_or(|unit| {
                        if let Some(points) = &unit.destinations {
                            let [x, y, _] = pose.position.map(f32::from_bits);
                            return points.is_empty()
                                || (points.len() == 1
                                    && points.front().is_some_and(|point| {
                                        (point[0] - x).hypot(point[1] - y) <= 0.05
                                    }));
                        }
                        unit.attack_target
                            .is_some_and(|(target, _)| transform_state(world, target).is_err())
                    });
            if complete {
                while let Some(order) = queue.pop_front() {
                    if self.start_script_order(world, actor, order) {
                        break;
                    }
                }
            }
            if !queue.is_empty() {
                self.script_orders.push((actor, queue));
            }
        }
    }

    pub(crate) fn script_order_counts(&self) -> impl Iterator<Item = (u32, u32, u64)> + '_ {
        self.script_order_counts
            .iter()
            .map(|(&(unit, order), &count)| (unit, order, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_world::{
        construct_object, register_object, request_delete, set_transform, ObjectDraft,
    };

    fn object(world: &mut World) -> ObjectHandle {
        let handle = construct_object(world, ObjectDraft { original_id: None }).unwrap();
        register_object(world, handle).unwrap();
        handle
    }

    fn place(world: &mut World, actor: ObjectHandle, x: f32) {
        let mut pose = transform_state(world, actor).unwrap();
        pose.position = [x, 0.0, 0.0].map(f32::to_bits);
        set_transform(world, actor, pose).unwrap();
    }

    #[test]
    fn move_attack_and_shutdown_wait_for_the_preceding_task() {
        let mut world = fparkan_world::new(Default::default());
        let actor = object(&mut world);
        let target = object(&mut world);
        let mut patrol = UnitPatrol::scripted();
        patrol.shut_down(actor);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Move([10.0, 0.0]), false);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Move([20.0, 0.0]), true);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Attack(target, 1), true);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Shutdown, true);
        patrol.advance_script_orders(&world);
        assert!(!patrol.is_shut_down(actor));
        assert!(patrol.attack_target(actor).is_none());
        assert_eq!(
            patrol.units[0].destinations.as_ref().unwrap()[0],
            [10.0, 0.0]
        );
        place(&mut world, actor, 10.0);
        patrol.advance_script_orders(&world);
        assert_eq!(
            patrol.units[0].destinations.as_ref().unwrap()[0],
            [20.0, 0.0]
        );
        place(&mut world, actor, 20.0);
        patrol.advance_script_orders(&world);
        assert_eq!(patrol.attack_target(actor), Some((target, 1)));
        assert!(!patrol.is_shut_down(actor));
        request_delete(&mut world, target).unwrap();
        fparkan_world::step(&mut world, &Default::default()).unwrap();
        patrol.advance_script_orders(&world);
        assert!(patrol.is_shut_down(actor));
        assert!(!patrol.is_patrolling(actor));
    }

    #[test]
    fn replacing_an_order_discards_the_queued_tail() {
        let mut world = fparkan_world::new(Default::default());
        let actor = object(&mut world);
        let mut patrol = UnitPatrol::scripted();
        patrol.enqueue_script_order(&world, actor, UnitOrder::Move([10.0, 0.0]), false);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Shutdown, true);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Move([20.0, 0.0]), false);
        place(&mut world, actor, 10.0);
        patrol.advance_script_orders(&world);
        assert!(!patrol.is_shut_down(actor));
        assert_eq!(
            patrol.units[0].destinations.as_ref().unwrap()[0],
            [20.0, 0.0]
        );
        assert!(patrol.script_orders.is_empty());
    }

    #[test]
    fn attack_holds_an_appended_patrol_until_the_target_is_removed() {
        let mut world = fparkan_world::new(Default::default());
        let actor = object(&mut world);
        let target = object(&mut world);
        let mut patrol = UnitPatrol::scripted();
        patrol.enqueue_script_order(&world, actor, UnitOrder::Attack(target, 1), false);
        patrol.enqueue_script_order(&world, actor, UnitOrder::Patrol([20.0, 30.0], 60.0), true);
        patrol.advance_script_orders(&world);
        assert_eq!(patrol.attack_target(actor), Some((target, 1)));
        request_delete(&mut world, target).unwrap();
        fparkan_world::step(&mut world, &Default::default()).unwrap();
        patrol.advance_script_orders(&world);
        assert!(patrol.has_point_order(actor, [20.0, 30.0]));
        assert!(patrol.attack_target(actor).is_none());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_mission_19_queues_and_starts_its_scripted_raid() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.04/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let loaded = engine.loaded.as_ref().unwrap();
        let actor = crate::script_patrol::warbot_by_logic_id(
            &loaded.mission,
            &loaded.object_drafts,
            &engine.world,
            15,
        )
        .unwrap();
        let before = transform_state(&engine.world, actor).unwrap().position;
        assert!(loaded.unit_patrol.as_ref().unwrap().is_shut_down(actor));
        let mut notices = Vec::new();
        for now in (0..=63_000).step_by(100) {
            notices.extend(crate::frame(&mut engine, now).unwrap().clan_notices);
        }
        let loaded = engine.loaded.as_ref().unwrap();
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        assert!(!patrol.is_shut_down(actor));
        assert!(patrol.is_moving_to_destination(actor));
        assert!(patrol.attack_target(actor).is_none());
        let queue = &patrol
            .script_orders
            .iter()
            .find(|(unit, _)| *unit == actor)
            .unwrap()
            .1;
        assert_eq!(queue.len(), 5);
        assert!(matches!(queue[0], UnitOrder::Move([1528.0, 855.0])));
        assert!(matches!(queue[1], UnitOrder::Attack(..)));
        assert!(matches!(queue[4], UnitOrder::Shutdown));
        assert!(notices.contains(&crate::MissionNotice::Information(0)));
        assert!(loaded
            .clan_ai_events
            .stops()
            .all(|(stop, _)| !stop.reason.starts_with("Handler(15)")));
        for now in (63_100..=70_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        assert_ne!(
            transform_state(&engine.world, actor).unwrap().position[..2],
            before[..2]
        );
    }
}
