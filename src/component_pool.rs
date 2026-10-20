//! Shared grouped, searchable component catalog for both pipeline outputs.
use crate::{PipelineCard, PipelinePoolEntry, PipelinePoolGroup, PipelinePoolRow};
use slint::{ModelRc, VecModel};

// Search only descriptive metadata; never serialize connections (which contain keys).
pub struct PoolItem {
    pub index: usize,
    pub group: usize,
    pub card: PipelineCard,
    pub search: String,
}
pub const GROUPS: [&str; 5] = [
    "Speech-to-text",
    "Language models",
    "Text processing",
    "Decision models",
    "Output components",
];
pub fn groups(items: &[PoolItem], query: &str, columns: usize) -> ModelRc<PipelinePoolGroup> {
    let query = query.to_lowercase();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let columns = columns.max(1);
    let groups = GROUPS
        .iter()
        .enumerate()
        .filter_map(|(group, title)| {
            let entries = items
                .iter()
                .filter(|item| {
                    item.group == group && terms.iter().all(|term| item.search.contains(term))
                })
                .map(|item| PipelinePoolEntry {
                    card: item.card.clone(),
                    catalog_index: item.index as i32,
                })
                .collect::<Vec<_>>();
            if entries.is_empty() {
                return None;
            }
            let rows = entries
                .chunks(columns)
                .map(|row| PipelinePoolRow {
                    entries: ModelRc::new(VecModel::from(row.to_vec())),
                })
                .collect::<Vec<_>>();
            Some(PipelinePoolGroup {
                title: (*title).into(),
                rows: ModelRc::new(VecModel::from(rows)),
            })
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(groups))
}
