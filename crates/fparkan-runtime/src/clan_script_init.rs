use crate::script_patrol::ScriptPatrol;
use fparkan_script::{ScriptDispatchSelector, ScriptInstruction, VarSet, VarSetDefault};

pub(super) enum InitOrder {
    Patrol(ScriptPatrol),
    Reserve {
        clan: usize,
        id: u32,
        reserved: bool,
    },
}

pub(super) fn reservation(
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
) -> Option<(u32, bool)> {
    if instruction.dispatch_selector() != ScriptDispatchSelector::Handler(51)
        || instruction.header_words[1] != u32::MAX
        || instruction.header_words[3] != u32::MAX
    {
        return None;
    }
    let (id, flag) = instruction.dword_pair(values)?;
    Some((id, flag != 0))
}

pub(super) fn patrol_order(
    declarations: &VarSet,
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
) -> Option<ScriptPatrol> {
    if instruction.dispatch_selector() != ScriptDispatchSelector::Handler(15)
        || instruction.header_words[1] != u32::MAX
        || instruction.header_words[3] != u32::MAX
    {
        return None;
    }
    ScriptPatrol::from_order(
        declarations
            .resolve_handler15_with_values(instruction, values)
            .ok()?,
    )
}

pub(super) fn stubbed_handler(instruction: &ScriptInstruction) -> Option<u8> {
    // shortcut: resume Init after the catalog refresh until the live research context is wired.
    (instruction.header_words == [43, u32::MAX, u32::MAX, u32::MAX, 6, 0, u32::MAX]
        && instruction.references.is_empty())
    .then_some(43)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_no_result_research_refresh_is_stubbed() {
        let mut instruction = ScriptInstruction {
            header_words: [43, u32::MAX, u32::MAX, u32::MAX, 6, 0, u32::MAX],
            references: Vec::new(),
        };
        assert_eq!(stubbed_handler(&instruction), Some(43));
        instruction.header_words[1] = 0;
        assert_eq!(stubbed_handler(&instruction), None);
    }

    #[test]
    fn reservation_reads_logical_id_and_treats_every_nonzero_flag_as_set() {
        let instruction = ScriptInstruction {
            header_words: [51, u32::MAX, u32::MAX, u32::MAX, 6, 2, u32::MAX],
            references: vec![1, 0],
        };
        assert_eq!(
            reservation(
                &instruction,
                &[VarSetDefault::Dword(2), VarSetDefault::Dword(12)]
            ),
            Some((12, true))
        );
        assert_eq!(
            reservation(
                &instruction,
                &[VarSetDefault::Dword(0), VarSetDefault::Dword(13)]
            ),
            Some((13, false))
        );
        assert_eq!(
            reservation(
                &instruction,
                &[VarSetDefault::FloatBits(0), VarSetDefault::Dword(13)]
            ),
            None
        );
    }
}
