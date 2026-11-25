use super::{
    advance_condition, EventInstruction, FormulaTable, InitInstructionResolution,
    ScriptDispatchSelector, ScriptEvent, ScriptInstruction, ScriptPackage, VarSet, VarSetDefault,
};

impl VarSet {
    /// Executes a single event with typed host results and output operands.
    ///
    /// # Errors
    ///
    /// Returns the first unsupported instruction, including transfers requiring a package.
    pub fn execute_event_prefix(
        &self,
        event: &ScriptEvent,
        formulas: Option<&FormulaTable>,
        values: &mut [VarSetDefault],
        host: impl FnMut(
            &ScriptInstruction,
            &mut [VarSetDefault],
        ) -> Result<Option<VarSetDefault>, String>,
    ) -> Result<(), (usize, String)> {
        match self.execute_event_segment(event, formulas, values, host)? {
            None => Ok(()),
            Some((index, target)) => Err((
                index,
                format!("event transfer to {target} requires a script package"),
            )),
        }
    }

    /// Executes an event and tail transfers within its package, preserving the live cells.
    /// The host receives the current event index, including after a transfer.
    ///
    /// # Errors
    ///
    /// Returns the actual event, instruction and reason for the first unsupported operation.
    pub fn execute_package_event_prefix(
        &self,
        package: &ScriptPackage,
        mut event_index: usize,
        formulas: Option<&FormulaTable>,
        values: &mut [VarSetDefault],
        mut host: impl FnMut(
            usize,
            &ScriptInstruction,
            &mut [VarSetDefault],
        ) -> Result<Option<VarSetDefault>, String>,
    ) -> Result<(), (usize, usize, String)> {
        loop {
            let event = package
                .events
                .get(event_index)
                .ok_or_else(|| (event_index, 0, "event is unavailable".into()))?;
            let transfer = self
                .execute_event_segment(event, formulas, values, |instruction, values| {
                    host(event_index, instruction, values)
                })
                .map_err(|(index, reason)| (event_index, index, reason))?;
            match transfer {
                None => return Ok(()),
                Some((index, target)) => {
                    if target >= package.events.len() {
                        return Err((
                            event_index,
                            index,
                            format!("event transfer target {target} is unavailable"),
                        ));
                    }
                    event_index = target;
                }
            }
        }
    }

    fn execute_event_segment(
        &self,
        event: &ScriptEvent,
        formulas: Option<&FormulaTable>,
        values: &mut [VarSetDefault],
        mut host: impl FnMut(
            &ScriptInstruction,
            &mut [VarSetDefault],
        ) -> Result<Option<VarSetDefault>, String>,
    ) -> Result<Option<(usize, usize)>, (usize, String)> {
        let mut conditions = Vec::new();
        let mut pc = 0;
        while let Some(instruction) = event.instructions.get(pc) {
            let index = pc;
            pc += 1;
            match advance_condition(&mut conditions, instruction, values)
                .map_err(|()| (index, "unsupported comparison operands".into()))?
            {
                EventInstruction::Skip => continue,
                EventInstruction::Return => break,
                EventInstruction::Execute => {}
            }
            let control = instruction.header_words[3];
            if control == 4 && instruction.dispatch_selector() == ScriptDispatchSelector::Sentinel {
                let &[target] = instruction.references.as_slice() else {
                    return Err((index, "event transfer requires one event index".into()));
                };
                return Ok(Some((index, target as usize)));
            }
            if control == 3 && instruction.dispatch_selector() == ScriptDispatchSelector::Sentinel {
                let &[target] = instruction.references.as_slice() else {
                    return Err((index, "jump requires one instruction index".into()));
                };
                pc = target as usize;
                if pc <= index || pc >= event.instructions.len() {
                    return Err((index, format!("unsupported jump to instruction {target}")));
                }
                conditions.clear();
                continue;
            }
            if instruction.dispatch_selector() != ScriptDispatchSelector::Sentinel {
                if control != u32::MAX {
                    return Err((index, format!("unsupported handler control {control}")));
                }
                let target = instruction.header_words[1];
                if target != u32::MAX && values.get(target as usize).is_none() {
                    return Err((index, format!("unsupported result target {target}")));
                }
                let result = host(instruction, values).map_err(|reason| (index, reason))?;
                if let Some(cell) = values.get_mut(target as usize) {
                    *cell = match (*cell, result) {
                        (VarSetDefault::Dword(_), Some(result @ VarSetDefault::Dword(_)))
                        | (
                            VarSetDefault::FloatBits(_),
                            Some(result @ VarSetDefault::FloatBits(_)),
                        ) => result,
                        (VarSetDefault::Dword(_), None) => VarSetDefault::Dword(0),
                        _ => return Err((index, format!("unsupported result target {target}"))),
                    };
                }
                continue;
            }
            match self
                .resolve_assignment(instruction, formulas, values)
                .map_err(|error| (index, error.to_string()))?
            {
                InitInstructionResolution::Executed(writes) => {
                    for write in writes {
                        *values.get_mut(write.index as usize).ok_or_else(|| {
                            (
                                index,
                                format!("varset index {} is out of bounds", write.index),
                            )
                        })? = write.value;
                    }
                }
                InitInstructionResolution::Unsupported(error) => {
                    return Err((index, error.to_string()))
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VarSetDeclaration, VarSetType};
    use std::sync::Arc;

    fn instruction(control: u32, target: u32, value: u32, references: &[u32]) -> ScriptInstruction {
        ScriptInstruction {
            header_words: [
                u32::MAX,
                target,
                value,
                control,
                6,
                references.len() as u32,
                u32::MAX,
            ],
            references: references.to_vec(),
        }
    }

    fn package(events: Vec<Vec<ScriptInstruction>>) -> ScriptPackage {
        ScriptPackage {
            opcode_handler_count: 73,
            events: events
                .into_iter()
                .map(|instructions| ScriptEvent {
                    name_len: 1,
                    name_raw: b"e\0".to_vec(),
                    event_word: 0,
                    instructions,
                })
                .collect(),
            trailing_bytes: Vec::new(),
            raw: Arc::from([]),
        }
    }

    fn variables() -> VarSet {
        VarSet {
            declarations: ["a", "b", "c"]
                .into_iter()
                .map(|name| VarSetDeclaration {
                    type_name: VarSetType::Dword,
                    name: name.into(),
                    default_value: VarSetDefault::Dword(0),
                })
                .collect(),
        }
    }

    #[test]
    fn event_transfer_keeps_cells_and_does_not_return_to_the_source() {
        let mut host_instruction = instruction(u32::MAX, u32::MAX, u32::MAX, &[]);
        host_instruction.header_words[0] = 0;
        let script = package(vec![
            vec![
                instruction(6, 0, 111, &[]),
                instruction(4, u32::MAX, u32::MAX, &[1]),
                instruction(6, 0, 999, &[]),
            ],
            vec![instruction(6, 0, 222, &[]), host_instruction],
        ]);
        let mut values = [0, 0, 0].map(VarSetDefault::Dword);
        let mut seen = Vec::new();
        variables()
            .execute_package_event_prefix(&script, 0, None, &mut values, |event, _, _| {
                seen.push(event);
                Ok(None)
            })
            .unwrap();
        assert_eq!(values[0], VarSetDefault::Dword(222));
        assert_eq!(seen, [1]);
    }

    #[test]
    fn false_branch_skips_transfer_and_destination_errors_keep_their_event() {
        let mut condition = instruction(0, u32::MAX, u32::MAX, &[0, 1]);
        condition.header_words[4] = 1;
        let mut host_instruction = instruction(u32::MAX, u32::MAX, u32::MAX, &[]);
        host_instruction.header_words[0] = 0;
        let script = package(vec![
            vec![
                condition,
                instruction(4, u32::MAX, u32::MAX, &[1]),
                instruction(6, 2, 333, &[]),
                instruction(1, u32::MAX, u32::MAX, &[]),
            ],
            vec![host_instruction],
        ]);
        let mut values = [17, 18, 0].map(VarSetDefault::Dword);
        variables()
            .execute_package_event_prefix(&script, 0, None, &mut values, |_, _, _| unreachable!())
            .unwrap();
        assert_eq!(values, [17, 18, 333].map(VarSetDefault::Dword));
        values[1] = values[0];
        assert_eq!(
            variables().execute_package_event_prefix(&script, 0, None, &mut values, |_, _, _| Err(
                "host stop".into()
            )),
            Err((1, 0, "host stop".into()))
        );
    }
}
