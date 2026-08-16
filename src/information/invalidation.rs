use std::collections::{BTreeMap, BTreeSet};

use super::{AtomIdV1, ProjectionReceiptIdV1};

#[derive(Clone, Debug, Default)]
pub struct ReceiptDependencyIndexV1 {
    by_atom: BTreeMap<AtomIdV1, BTreeSet<ProjectionReceiptIdV1>>,
}

impl ReceiptDependencyIndexV1 {
    pub fn insert(
        &mut self,
        receipt: ProjectionReceiptIdV1,
        read_set: impl IntoIterator<Item = AtomIdV1>,
    ) {
        for atom in read_set {
            self.by_atom
                .entry(atom)
                .or_default()
                .insert(receipt.clone());
        }
    }

    pub fn invalidated_by(
        &self,
        changed: impl IntoIterator<Item = AtomIdV1>,
    ) -> BTreeSet<ProjectionReceiptIdV1> {
        changed
            .into_iter()
            .filter_map(|atom| self.by_atom.get(&atom))
            .flatten()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_receipts_that_read_changed_atoms_are_invalidated() {
        let first_atom = AtomIdV1::from_sha256("11".repeat(32)).unwrap();
        let second_atom = AtomIdV1::from_sha256("22".repeat(32)).unwrap();
        let first_receipt = ProjectionReceiptIdV1::from_sha256("33".repeat(32)).unwrap();
        let second_receipt = ProjectionReceiptIdV1::from_sha256("44".repeat(32)).unwrap();
        let mut index = ReceiptDependencyIndexV1::default();
        index.insert(first_receipt.clone(), [first_atom.clone()]);
        index.insert(second_receipt, [second_atom]);
        assert_eq!(index.invalidated_by([first_atom]), [first_receipt].into());
    }
}
