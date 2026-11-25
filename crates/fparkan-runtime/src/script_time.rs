use fparkan_script::{ScriptDispatchSelector, ScriptInitWrite, ScriptInstruction, VarSetDefault};

pub(super) fn deadline_write(
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
) -> Option<ScriptInitWrite> {
    let index = instruction.header_words[1];
    if instruction.dispatch_selector() != ScriptDispatchSelector::Handler(59)
        || instruction.header_words[3] != u32::MAX
        || !matches!(values.get(index as usize), Some(VarSetDefault::Dword(_)))
    {
        return None;
    }
    Some(ScriptInitWrite {
        index,
        value: VarSetDefault::Dword(result(instruction, values, 0)?),
    })
}

pub(super) fn result(
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
    clock: u32,
) -> Option<u32> {
    let &[index] = instruction.references.as_slice() else {
        return None;
    };
    let &VarSetDefault::Dword(value) = values.get(index as usize)? else {
        return None;
    };
    match instruction.dispatch_selector() {
        ScriptDispatchSelector::Handler(59) => Some(clock.wrapping_add(value)),
        ScriptDispatchSelector::Handler(60) => Some(if value < clock { 1 } else { u32::MAX }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_and_strict_timeout_match_native_handlers() {
        let mut instruction = ScriptInstruction {
            header_words: [59, 0, u32::MAX, u32::MAX, 6, 1, u32::MAX],
            references: vec![0],
        };
        assert_eq!(
            deadline_write(&instruction, &[VarSetDefault::Dword(45)]),
            Some(ScriptInitWrite {
                index: 0,
                value: VarSetDefault::Dword(45)
            })
        );
        assert_eq!(
            result(&instruction, &[VarSetDefault::Dword(45)], 7),
            Some(52)
        );
        assert_eq!(
            result(&instruction, &[VarSetDefault::Dword(2)], u32::MAX),
            Some(1)
        );
        instruction.header_words[0] = 60;
        let values = [VarSetDefault::Dword(45)];
        assert_eq!(result(&instruction, &values, 44), Some(u32::MAX));
        assert_eq!(result(&instruction, &values, 45), Some(u32::MAX));
        assert_eq!(result(&instruction, &values, 46), Some(1));
    }
}
