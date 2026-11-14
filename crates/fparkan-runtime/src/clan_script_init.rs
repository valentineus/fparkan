use fparkan_script::ScriptInstruction;

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
