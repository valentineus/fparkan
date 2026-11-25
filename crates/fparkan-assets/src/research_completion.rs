use super::{ResearchError, ResearchTrfDocument, TRF1};

/// Changes produced by native research completion and its free descendants.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResearchCompletion {
    /// Newly learned rows in native completion order.
    pub learned: Vec<usize>,
    /// Newly available rows in native scan order.
    pub available: Vec<usize>,
}

impl ResearchTrfDocument {
    /// Completes one row, updates prerequisite gates and completes free descendants.
    ///
    /// # Errors
    /// Rejects invalid row links and a cycle among unfinished free descendants.
    pub fn complete_research(&mut self, index: usize) -> Result<ResearchCompletion, ResearchError> {
        let mut result = ResearchCompletion::default();
        self.mark_researched(index, &mut result)?;
        let mut stack = vec![(index, 0)];
        while let Some((row, next)) = stack.last_mut() {
            let Some(&child) = self.relation_4_5(*row)?.get(*next) else {
                stack.pop();
                continue;
            };
            *next += 1;
            let child = child as usize;
            let costs = &self.records[child];
            if self.flags[child] & 3 != 1
                || f32::from_bits(costs[0]) != 0.0
                || f32::from_bits(costs[1]) != 0.0
            {
                continue;
            }
            if stack.iter().any(|&(row, _)| row == child) {
                return Err(ResearchError::CyclicResearchCompletion { index: child });
            }
            self.mark_researched(child, &mut result)?;
            stack.push((child, 0));
        }
        Ok(result)
    }

    fn mark_researched(
        &mut self,
        index: usize,
        result: &mut ResearchCompletion,
    ) -> Result<(), ResearchError> {
        let previous = self
            .flag_byte(index)
            .ok_or(ResearchError::InvalidResearchIndex {
                type_id: TRF1,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                row_count: self.flags.len(),
            })?;
        if previous & 4 == 0 {
            return Ok(());
        }
        self.flags[index] |= 3;
        if previous & 2 == 0 {
            result.learned.push(index);
        }
        if previous & 1 == 0 {
            result.available.push(index);
        }
        for row in 0..self.records.len() {
            if self.flags[row] & 5 == 4
                && self
                    .relation_2_3(row)?
                    .iter()
                    .all(|&required| self.flags[required as usize] & 6 == 6)
            {
                self.flags[row] |= 1;
                result.available.push(row);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{ResearchRelations, TRF2, TRF4};
    use std::sync::Arc;

    fn relations(id: u32, groups: &[&[u32]]) -> ResearchRelations {
        let mut group_starts = vec![0];
        let mut indices = Vec::new();
        for group in groups {
            indices.extend_from_slice(group);
            group_starts.push(indices.len());
        }
        ResearchRelations {
            table_id: id,
            group_starts,
            indices,
        }
    }

    fn fixture() -> ResearchTrfDocument {
        let archive =
            crate::decode_nres_payload(Arc::from(&b"NRes\0\x01\0\0\0\0\0\0\x10\0\0\0"[..]))
                .unwrap();
        ResearchTrfDocument {
            archive,
            records: vec![[0; 10]; 6],
            flags: vec![0x85, 4, 4, 0, 5, 5],
            aliases: vec![],
            name_pool: vec![],
            relation_2_3: relations(TRF2, &[&[], &[0], &[0, 3], &[], &[], &[1]]),
            relation_4_5: relations(TRF4, &[&[], &[], &[], &[], &[], &[]]),
        }
    }

    #[test]
    fn native_mark_requires_enabled_prerequisites_and_preserves_other_flag_bits() {
        let mut table = fixture();
        let changes = table.complete_research(0).unwrap();
        assert_eq!(table.flags, [0x87, 5, 4, 0, 5, 5]);
        assert_eq!(changes.learned, [0]);
        assert_eq!(changes.available, [1]);
        let mut disabled = fixture();
        disabled.flags[0] = 1;
        assert_eq!(
            disabled.complete_research(0).unwrap(),
            ResearchCompletion::default()
        );
        assert_eq!(disabled.flags, [1, 4, 4, 0, 5, 5]);
    }

    #[test]
    fn native_completion_recurses_only_into_available_zero_cost_descendants() {
        let mut table = fixture();
        table.flags = vec![5, 4, 4, 4, 4, 4];
        table.relation_2_3 = relations(TRF2, &[&[], &[0], &[0], &[], &[], &[1]]);
        table.relation_4_5 = relations(TRF4, &[&[1, 2, 4], &[5], &[], &[], &[], &[]]);
        table.records[2][0] = 1.0_f32.to_bits();
        table.records[4][0] = (-1.0_f32).to_bits();
        let changes = table.complete_research(0).unwrap();
        assert_eq!(table.flags, [7, 7, 5, 5, 5, 7]);
        assert_eq!(changes.learned, [0, 1, 5]);
        assert_eq!(changes.available, [1, 2, 3, 4, 5]);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_tables_match_native_completion_changes() {
        let root = std::path::PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap());
        for (file, row, learned, available) in [
            ("scream.trf", 19, &[19][..], &[][..]),
            ("scream.trf", 20, &[20, 68, 69][..], &[68, 69][..]),
            ("c4m2e2.trf", 3, &[3][..], &[][..]),
        ] {
            let bytes = std::fs::read(root.join("MISSIONS/SCRIPTS").join(file)).unwrap();
            let mut table = ResearchTrfDocument::decode(Arc::from(bytes)).unwrap();
            let mut changes = table.complete_research(row).unwrap();
            changes.learned.sort_unstable();
            changes.available.sort_unstable();
            assert_eq!(changes.learned, learned, "{file} row {row}: learned");
            assert_eq!(changes.available, available, "{file} row {row}: available");
        }
    }
}
