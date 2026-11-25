use crate::{mission_events::MissionEvents, MissionDocument, MissionObjectDraft};
use fparkan_world::{identity_metadata, World};

pub(super) fn query(
    mission: &MissionDocument,
    roster: &MissionEvents,
    drafts: &[MissionObjectDraft],
    world: &World,
    id: u32,
) -> Result<u32, String> {
    let Some(unit) = roster.logic_objects(mission, world, id).next() else {
        return Ok(u32::MAX);
    };
    let draft = identity_metadata(world, unit)
        .ok()
        .and_then(|identity| identity.original_id)
        .and_then(|id| drafts.get(id.0 as usize))
        .ok_or("Handler(72) unit descriptor is unavailable")?;
    let resource = &draft
        .unit_components
        .first()
        .ok_or("Handler(72) root prototype is unavailable")?
        .resource_raw;
    // shortcut: infer the descriptor kind from Unit DAT until native descriptor interfaces are wired.
    let kind = match draft.unit_dat_header_flags {
        Some(class) if class & 0x8000_0000 != 0 || class == 0x0101_0000 => 3,
        Some(class) if class & 0x0100_0000 != 0 => 4,
        _ => return Err("Handler(72) descriptor kind is unavailable".into()),
    };
    Ok(classify(kind, resource))
}

fn classify(kind: u32, name: &[u8]) -> u32 {
    let name = &name[..name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len())];
    let index = match kind {
        3 => 3,
        4 => 2,
        _ => return 0,
    };
    match (kind, name.get(index).map(u8::to_ascii_lowercase)) {
        (4, Some(b't')) => 1,
        (3 | 4, Some(b'l')) | (4, Some(b'h')) => 2,
        (3 | 4, Some(b'm')) => 3,
        (3 | 4, Some(b'b')) => 4,
        (3, Some(b'e')) => 5,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_descriptor_classes_use_the_kind_and_fixed_character() {
        for (kind, name, expected) in [
            (4, "R_T_02", 1),
            (4, "r_l_01", 2),
            (4, "r_h_03", 2),
            (4, "r_m_02", 3),
            (4, "r_b_01", 4),
            (4, "r_e_01", 0),
            (3, "fr_l_bunker", 2),
            (3, "fr_m_cargo", 3),
            (3, "fr_b_plant", 4),
            (3, "fr_e_lab", 5),
            (3, "fr_h_invalid", 0),
            (3, "fr_t_invalid", 0),
            (3, "xx_m", 3),
            (2, "r_l_01", 0),
            (4, "r", 0),
        ] {
            assert_eq!(classify(kind, name.as_bytes()), expected, "{kind} {name}");
        }
        assert_eq!(classify(4, b"r_\0m"), 0);
    }
}
