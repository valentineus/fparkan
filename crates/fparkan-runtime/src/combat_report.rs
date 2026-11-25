use fparkan_world::{identity_metadata, ObjectHandle, World};
use std::collections::BTreeMap;

/// Identity retained by combat diagnostics after an object has been removed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CombatObject {
    pub slot: u32,
    pub generation: u32,
    pub original_id: Option<u32>,
}

impl CombatObject {
    pub(super) fn read(world: &World, handle: ObjectHandle) -> Self {
        Self {
            slot: handle.slot,
            generation: handle.generation,
            original_id: identity_metadata(world, handle)
                .ok()
                .and_then(|identity| identity.original_id)
                .map(|id| id.0),
        }
    }
}

impl std::fmt::Display for CombatObject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.slot, self.generation)
    }
}

/// Event count and endpoints on the mission clock, including time zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CombatOccurrences {
    pub count: u64,
    pub first_ms: u32,
    pub last_ms: u32,
}

fn record<K: Ord>(
    counts: &mut BTreeMap<K, CombatOccurrences>,
    key: K,
    now: u32,
    count: u64,
) -> bool {
    let first = !counts.contains_key(&key);
    let total = counts.entry(key).or_insert(CombatOccurrences {
        count: 0,
        first_ms: now,
        last_ms: now,
    });
    total.count += count;
    total.last_ms = now;
    first
}

/// Autonomous combat observations shared by the window and headless runtime.
/// Empty weapon banks are included as availability diagnostics, not proof of a missing rule.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CombatReport {
    pub shots: BTreeMap<(CombatObject, CombatObject), CombatOccurrences>,
    pub destroyed: BTreeMap<(CombatObject, CombatObject), CombatOccurrences>,
    pub unavailable: BTreeMap<(CombatObject, &'static str), CombatOccurrences>,
}

impl CombatReport {
    pub(super) fn shot(&mut self, shooter: CombatObject, target: CombatObject, now: u32) {
        if record(&mut self.shots, (shooter, target), now, 1) {
            eprintln!("unit_combat_first_shot_ms={now} shooter={shooter} target={target} shooter_id={:?} target_id={:?}", shooter.original_id, target.original_id);
        }
    }

    pub(super) fn destroyed(&mut self, shooter: CombatObject, target: CombatObject, now: u32) {
        if record(&mut self.destroyed, (shooter, target), now, 1) {
            eprintln!("unit_combat_first_destroyed_ms={now} shooter={shooter} target={target} shooter_id={:?} target_id={:?}", shooter.original_id, target.original_id);
        }
    }

    pub(super) fn unavailable(
        &mut self,
        object: CombatObject,
        reason: &'static str,
        now: u32,
        count: u64,
    ) -> bool {
        let first = record(&mut self.unavailable, (object, reason), now, count);
        if first {
            eprintln!("unit_combat_unavailable={count} object={object} original_id={:?} reason={reason} first_ms={now}", object.original_id);
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_zero_time_and_separates_reused_slots_and_failure_reasons() {
        let shooter = CombatObject {
            slot: 7,
            generation: 0,
            original_id: Some(19),
        };
        let target = CombatObject {
            slot: 9,
            generation: 0,
            original_id: Some(23),
        };
        let replacement = CombatObject {
            generation: 1,
            ..target
        };
        let mut report = CombatReport::default();
        report.shot(shooter, target, 0);
        report.shot(shooter, target, 50);
        report.destroyed(shooter, target, 100);
        report.shot(shooter, replacement, 150);
        report.unavailable(shooter, "projectile_contact", 160, 2);
        report.unavailable(shooter, "projectile_contact", 170, 3);
        report.unavailable(shooter, "projectile_advance", 170, 1);
        assert_eq!(
            report.shots[&(shooter, target)],
            CombatOccurrences {
                count: 2,
                first_ms: 0,
                last_ms: 50
            }
        );
        assert_eq!(report.shots[&(shooter, replacement)].first_ms, 150);
        assert_eq!(report.destroyed[&(shooter, target)].count, 1);
        assert_eq!(
            report.unavailable[&(shooter, "projectile_contact")],
            CombatOccurrences {
                count: 5,
                first_ms: 160,
                last_ms: 170
            }
        );
        assert_eq!(
            report.unavailable[&(shooter, "projectile_advance")].count,
            1
        );
    }
}
