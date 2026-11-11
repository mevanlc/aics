use std::collections::BTreeMap;
use std::path::Path;

use aics::lifecycle::{self, DependencyGraph, Operation};
use aics::parse::Agent;
use aics::scan::SessionRoots;
use aics::trash::{TrashItem, TrashReason, TrashStore};
use anyhow::{Context, Result};

pub fn trash_session(
    roots: &SessionRoots,
    path: &Path,
    agent: Agent,
    reason: TrashReason,
) -> Result<TrashItem> {
    let original = path.canonicalize()?;
    let path = path.to_path_buf();
    let plan = DependencyGraph::discover(roots, agent, std::slice::from_ref(&path))?
        .plan([path.clone()].into(), Operation::Trash);
    let reasons = BTreeMap::from([(path.clone(), reason)]);
    lifecycle::execute(roots, &plan, &reasons)?;
    TrashStore::new(roots.trash.clone().context("trash store")?)
        .snapshot()?
        .into_iter()
        .find(|i| i.original_path().as_ref() == Some(&original))
        .context("trashed fixture")
}
