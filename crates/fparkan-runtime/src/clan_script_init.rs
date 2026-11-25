use crate::script_patrol::ScriptPatrol;
use fparkan_script::{
    Handler15TargetPayload, ScriptDispatchSelector, ScriptInstruction, VarSet, VarSetDefault,
};

pub(super) enum InitOrder {
    BaseRadius {
        clan: usize,
        limit: f32,
    },
    Patrol(ScriptPatrol),
    Unit(fparkan_script::Handler15Invocation),
    Reserve {
        clan: usize,
        id: u32,
        reserved: bool,
    },
}

pub(super) fn base_radius(
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
) -> Option<f32> {
    let ([68, u32::MAX, u32::MAX, u32::MAX, 6, 1, u32::MAX], &[index]) =
        (instruction.header_words, instruction.references.as_slice())
    else {
        return None;
    };
    values.get(index as usize)?.as_float()
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

pub(super) fn unit_order(
    declarations: &VarSet,
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
) -> Option<InitOrder> {
    if instruction.dispatch_selector() != ScriptDispatchSelector::Handler(15)
        || instruction.header_words[1] != u32::MAX
        || instruction.header_words[3] != u32::MAX
    {
        return None;
    }
    let order = declarations
        .resolve_handler15_with_values(instruction, values)
        .ok()?;
    if crate::unit_patrol::supports_order(&order) {
        return Some(InitOrder::Unit(order));
    }
    ScriptPatrol::from_order(order).map(InitOrder::Patrol)
}

pub(super) fn stubbed_handler(
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
    declarations: &VarSet,
) -> Option<u8> {
    match (instruction.header_words, instruction.references.as_slice()) {
        // shortcut: resume Init until the live research context is wired.
        ([43, u32::MAX, u32::MAX, u32::MAX, 6, 0, u32::MAX], []) => Some(43),
        // shortcut: resume Init until the AI consumer of this setting is wired.
        ([69, u32::MAX, u32::MAX, u32::MAX, 6, 1, u32::MAX], [index])
            if matches!(values.get(*index as usize), Some(VarSetDefault::Dword(_))) =>
        {
            Some(69)
        }
        // shortcut: resume Init until the search and capture task is wired.
        ([15, u32::MAX, u32::MAX, u32::MAX, 6, 10, u32::MAX], _) => {
            let order = declarations
                .resolve_handler15_with_values(instruction, values)
                .ok()?;
            matches!(
                (order.word_1, order.word_2, order.target),
                (5, 3, Handler15TargetPayload::ByType(0x8000_0002))
            )
            .then_some(15)
        }
        _ => None,
    }
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
        let declarations = fparkan_script::parse_varset(b"").unwrap();
        assert_eq!(stubbed_handler(&instruction, &[], &declarations), Some(43));
        instruction.header_words[1] = 0;
        assert_eq!(stubbed_handler(&instruction, &[], &declarations), None);
    }

    #[test]
    fn base_area_requires_a_float_operand_and_no_result() {
        let mut instruction = ScriptInstruction {
            header_words: [68, u32::MAX, u32::MAX, u32::MAX, 6, 1, u32::MAX],
            references: vec![0],
        };
        let values = [VarSetDefault::FloatBits(500.0_f32.to_bits())];
        assert_eq!(base_radius(&instruction, &values), Some(500.0));
        assert_eq!(
            base_radius(&instruction, &[VarSetDefault::Dword(500)]),
            None
        );
        instruction.header_words[1] = 0;
        assert_eq!(base_radius(&instruction, &values), None);
    }

    #[test]
    fn ai_setting_stub_requires_a_dword_operand_and_no_result() {
        let mut instruction = ScriptInstruction {
            header_words: [69, u32::MAX, u32::MAX, u32::MAX, 6, 1, u32::MAX],
            references: vec![0],
        };
        let values = [VarSetDefault::Dword(15)];
        let declarations = fparkan_script::parse_varset(b"").unwrap();
        assert_eq!(
            stubbed_handler(&instruction, &values, &declarations),
            Some(69)
        );
        assert_eq!(
            stubbed_handler(&instruction, &[VarSetDefault::FloatBits(0)], &declarations),
            None
        );
        instruction.header_words[1] = 0;
        assert_eq!(stubbed_handler(&instruction, &values, &declarations), None);
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
