use crate::script_patrol::ScriptPatrol;
use fparkan_script::{ScriptDispatchSelector, ScriptInstruction, VarSet, VarSetDefault};

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
}
