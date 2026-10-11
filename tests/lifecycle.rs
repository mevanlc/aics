use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use aics::lifecycle::{self, DependencyGraph, DependencyPolicy, LifecyclePlan, Operation};
use aics::parse::Agent;
use aics::rules::{apply_rule_proposals, RuleAction, RuleProposal};
use aics::scan::SessionRoots;
use aics::trash::{TrashPaths, TrashReason, TrashStore};
use anyhow::Result;
use tempfile::TempDir;

#[path = "common/lock_holder.rs"]
mod lock_holder;

struct Fixture {
    _temp: TempDir,
    roots: SessionRoots,
}
impl Fixture {
    fn new() -> Result<Self> {
        let temp = TempDir::new()?;
        let roots = SessionRoots {
            live_sessions: Default::default(),
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex/sessions"),
            antigravity_home: temp.path().join("antigravity"),
            trash: Some(TrashPaths::from_data_root(temp.path().join("data"))),
        };
        fs::create_dir_all(&roots.codex_sessions)?;
        Ok(Self { _temp: temp, roots })
    }
    fn rollout(&self, thread: &str, rollout: &str, parent: Option<&str>) -> Result<PathBuf> {
        let suffix = if thread == rollout {
            thread.to_owned()
        } else {
            format!("{thread}_{rollout}")
        };
        let path = self
            .roots
            .codex_sessions
            .join(format!("rollout-2026-09-30T11-55-37-{suffix}.jsonl"));
        let mut payload =
            serde_json::json!({"id":thread,"cwd":"/tmp/test","history_mode":"paginated"});
        if let Some(parent) = parent {
            payload["history_base"] = serde_json::json!({"thread_id":parent,"end_ordinal_exclusive":10,"end_byte_offset":100});
        }
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_meta","payload":payload}),
                serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}})
            ),
        )?;
        Ok(path)
    }
    fn plan(&self, seeds: &[PathBuf], op: Operation) -> Result<LifecyclePlan> {
        Ok(DependencyGraph::discover(&self.roots, Agent::Codex, seeds)?
            .plan(seeds.iter().cloned().collect(), op))
    }
    fn execute(&self, plan: &LifecyclePlan) -> Result<()> {
        lifecycle::execute(
            &self.roots,
            plan,
            &plan
                .members
                .keys()
                .map(|p| (p.clone(), TrashReason::KeyBinding { key: "^T".into() }))
                .collect(),
        )
    }
    fn trash_path(&self, path: &Path) -> PathBuf {
        self.roots
            .trash
            .as_ref()
            .unwrap()
            .trash_dir
            .join(path.file_name().unwrap())
    }
}

#[test]
fn legacy_codex_header_supports_dependency_discovery_and_restore() -> Result<()> {
    let f = Fixture::new()?;
    let legacy = f.rollout("legacy", "legacy", None)?;
    fs::write(
        &legacy,
        "{\"id\":\"legacy\",\"timestamp\":\"2025-08-01T00:00:00Z\",\"instructions\":\"\"}\n\
         {\"record_type\":\"state\"}\n\
         {\"type\":\"message\",\"id\":\"message-id\",\"role\":\"user\",\"content\":[]}\n",
    )?;
    let child = f.rollout("child", "child", Some("legacy"))?;
    let plan = f.plan(std::slice::from_ref(&legacy), Operation::Trash)?;
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert_eq!(plan.members.len(), 2);
    assert_eq!(plan.members[&legacy].thread_id, "legacy");
    f.execute(&plan)?;
    let restore = f.plan(&[f.trash_path(&child)], Operation::Restore)?;
    assert!(restore.blockers.is_empty(), "{:?}", restore.blockers);
    assert_eq!(restore.members.len(), 2);
    f.execute(&restore)?;
    assert!(legacy.exists() && child.exists());
    Ok(())
}

#[test]
fn replacement_rollouts_move_and_restore_as_one_thread() -> Result<()> {
    let f = Fixture::new()?;
    let original = f.rollout("thread", "thread", None)?;
    let replacement = f.rollout("thread", "replacement", Some("thread"))?;
    let plan = f.plan(std::slice::from_ref(&replacement), Operation::Trash)?;
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert_eq!(plan.session_count(), 1);
    assert_eq!(plan.members.len(), 2);
    assert!(plan.expanded());
    f.execute(&plan)?;
    assert!(!original.exists() && !replacement.exists());
    let restore = f.plan(&[f.trash_path(&replacement)], Operation::Restore)?;
    assert_eq!(restore.members.len(), 2);
    f.execute(&restore)?;
    assert!(original.exists() && replacement.exists());
    assert!(TrashStore::new(f.roots.trash.clone().unwrap())
        .reasons()?
        .is_empty());
    Ok(())
}

#[test]
fn transitive_dependents_are_required_but_ancestors_and_siblings_can_remain() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let c = f.rollout("c", "c", Some("b"))?;
    let sibling = f.rollout("sibling", "sibling", Some("a"))?;
    let plan = f.plan(std::slice::from_ref(&b), Operation::Trash)?;
    assert_eq!(
        plan.members.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([b.clone(), c])
    );
    f.execute(&plan)?;
    assert!(a.exists() && sibling.exists());
    let restore = f.plan(&[f.trash_path(&b)], Operation::Restore)?;
    assert_eq!(restore.members.len(), 1);
    f.execute(&restore)?;
    Ok(())
}

#[test]
fn fork_metadata_is_context_not_a_storage_dependency() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.rollout("parent", "parent", None)?;
    let fork = f.rollout("fork", "fork", None)?;
    fs::write(
        &fork,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"fork\",\"forked_from_id\":\"parent\"}}\n",
    )?;
    let graph = DependencyGraph::discover(&f.roots, Agent::Codex, std::slice::from_ref(&parent))?;
    let plan = graph.plan(BTreeSet::from([parent]), Operation::Trash);
    assert_eq!(plan.members.len(), 1);
    assert_eq!(graph.related(&plan.seeds).len(), 2);
    Ok(())
}

#[test]
fn dependency_resolves_replacement_id_in_archived_compressed_rollout() -> Result<()> {
    let f = Fixture::new()?;
    let parent = f.rollout("parent", "replacement", None)?;
    let child = f.rollout("child", "child", Some("replacement"))?;
    let archive = f
        .roots
        .codex_sessions
        .parent()
        .unwrap()
        .join("archived_sessions");
    fs::create_dir(&archive)?;
    let compressed = archive.join(format!(
        "{}.zst",
        child.file_name().unwrap().to_string_lossy()
    ));
    fs::write(
        &compressed,
        zstd::stream::encode_all(fs::File::open(&child)?, 1)?,
    )?;
    fs::remove_file(child)?;
    let parsed = aics::parse::parse_session_file(Agent::Codex, &compressed)?.unwrap();
    assert_eq!(
        parsed.lineage.history_base_rollout_id.as_deref(),
        Some("replacement")
    );
    let parsed_parent = aics::parse::parse_session_file(Agent::Codex, &parent)?.unwrap();
    assert_eq!(
        parsed_parent.lineage.rollout_id.as_deref(),
        Some("replacement")
    );
    let plan = f.plan(&[parent], Operation::Trash)?;
    assert!(plan.members.contains_key(&compressed));
    assert_eq!(plan.members.len(), 2);
    f.execute(&plan)?;
    assert!(f.trash_path(&compressed).exists());
    Ok(())
}

#[test]
fn restore_follows_missing_ancestors_and_refuses_collisions() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    f.execute(&f.plan(std::slice::from_ref(&a), Operation::Trash)?)?;
    let plan = f.plan(&[f.trash_path(&b)], Operation::Restore)?;
    assert_eq!(plan.members.len(), 2);
    fs::write(&a, "external file")?;
    assert!(f.execute(&plan).is_err());
    assert_eq!(fs::read_to_string(&a)?, "external file");
    fs::remove_file(&a)?;
    fs::remove_file(f.trash_path(&a))?;
    let broken = f.plan(&[f.trash_path(&b)], Operation::Restore)?;
    assert!(broken
        .blockers
        .iter()
        .any(|s| s.contains("missing required history")));
    Ok(())
}

fn proposal(path: PathBuf, policy: DependencyPolicy) -> RuleProposal {
    RuleProposal {
        reviewed_group: None,
        dependency_policy: policy,
        included_by: None,
        rule: "cleanup".into(),
        session_id: "a".into(),
        path,
        agent: Agent::Codex,
        action: RuleAction::Trash {
            reason: Some("test".into()),
        },
    }
}

#[test]
fn rules_skip_unsafe_singles_and_include_unmatched_dependents() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let (applied, skipped) =
        apply_rule_proposals(&f.roots, &[proposal(a.clone(), DependencyPolicy::Skip)]);
    assert!(applied.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(a.exists() && b.exists());
    let (applied, skipped) =
        apply_rule_proposals(&f.roots, &[proposal(a.clone(), DependencyPolicy::Include)]);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(applied.len(), 2);
    assert!(!a.exists() && !b.exists());
    let reasons = TrashStore::new(f.roots.trash.clone().unwrap()).reasons()?;
    assert!(reasons
        .values()
        .all(|r| matches!(r.reason, TrashReason::DependencyGroup { .. })));
    Ok(())
}

#[test]
fn complete_skip_selections_execute_as_one_group() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let (applied, skipped) = apply_rule_proposals(
        &f.roots,
        &[
            proposal(a, DependencyPolicy::Skip),
            proposal(b, DependencyPolicy::Skip),
        ],
    );
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(applied.len(), 2);
    Ok(())
}

#[test]
fn stale_approved_plan_cannot_remove_new_dependent() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let plan = f.plan(std::slice::from_ref(&a), Operation::Trash)?;
    let b = f.rollout("b", "b", Some("a"))?;
    assert!(f.execute(&plan).is_err());
    assert!(a.exists() && b.exists());
    Ok(())
}

#[test]
fn locked_member_blocks_entire_group_and_coordination_is_acquired_once() -> Result<()> {
    let mut f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let locks = f
        .roots
        .codex_sessions
        .parent()
        .unwrap()
        .join("thread-writer-locks");
    fs::create_dir(&locks)?;
    fs::write(locks.join(".coordination.lock"), "")?;
    fs::write(locks.join("a.lock"), "")?;
    fs::write(locks.join("b.lock"), "")?;
    f.roots.live_sessions.codex_writer_locks_dir = Some(locks.clone());
    let held = lock_holder::HeldLock::new(&locks.join("b.lock"));
    let plan = f.plan(std::slice::from_ref(&a), Operation::Trash)?;
    assert!(f.execute(&plan).is_err());
    assert!(a.exists() && b.exists());
    drop(held);
    f.execute(&plan)?;
    assert!(!a.exists() && !b.exists());
    Ok(())
}

#[test]
fn purge_will_not_expand_into_active_dependents() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    f.execute(&f.plan(std::slice::from_ref(&a), Operation::Trash)?)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let plan = f.plan(&[f.trash_path(&a)], Operation::Delete)?;
    assert!(plan
        .blockers
        .iter()
        .any(|s| s.contains("must be trashed first")));
    assert!(f.execute(&plan).is_err());
    assert!(b.exists());
    Ok(())
}

#[test]
fn malformed_metadata_and_duplicate_rollout_identity_block_removal() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let bad = f.roots.codex_sessions.join("bad.jsonl");
    fs::write(&bad, "{}\n")?;
    assert!(!f
        .plan(std::slice::from_ref(&a), Operation::Trash)?
        .blockers
        .is_empty());
    fs::remove_file(&bad)?;
    fs::copy(&a, f.roots.codex_sessions.join("duplicate.jsonl"))?;
    assert!(f
        .plan(&[a], Operation::Trash)?
        .blockers
        .iter()
        .any(|s| s.contains("ambiguous")));
    Ok(())
}

#[test]
fn graph_discovery_is_read_only() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let _ = f.plan(&[a], Operation::Trash)?;
    assert!(!f.roots.trash.as_ref().unwrap().data_root.exists());
    Ok(())
}

#[test]
fn configured_include_preview_expands_outside_filters_and_requires_all_marked_members() -> Result<()>
{
    use aics::index::{Scope, SearchFilters};
    use aics::rules::{run_rules_with_progress, RuleSelection, RulesMode, RulesOptions};
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let rules_path = f._temp.path().join("rules.js");
    fs::write(
        &rules_path,
        r#"rule("cleanup", {applyAtStartup: true, dependencyPolicy: "include"}, ({session}) => session.id === "a" ? trash("done") : nothing());"#,
    )?;
    let options = RulesOptions {
        rules_path,
        cache_path: Some(f._temp.path().join("cache.json")),
        mode: RulesMode::Preview,
        selection: RuleSelection::ApplyAtStartup,
        json: false,
        scope: Scope::Global,
        filters: SearchFilters {
            session_id: Some("a".into()),
            ..Default::default()
        },
        supersession: Default::default(),
    };
    let report = run_rules_with_progress(&f.roots, &options, |_| {})?;
    assert_eq!(report.proposals.len(), 2);
    assert_eq!(report.preview_matches.len(), 2);
    let required = report.proposals.iter().find(|p| p.path == b).unwrap();
    assert_eq!(required.included_by.as_ref(), Some(&a));
    let (applied, skipped) = apply_rule_proposals(
        &f.roots,
        &report
            .proposals
            .iter()
            .filter(|p| p.path == a)
            .cloned()
            .collect::<Vec<_>>(),
    );
    assert!(applied.is_empty());
    assert!(skipped[0].skip_reason.contains("unmarked"));
    assert!(a.exists() && b.exists());
    let (applied, skipped) = apply_rule_proposals(&f.roots, &report.proposals);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(applied.len(), 2);
    Ok(())
}

#[test]
fn opposing_rules_cannot_restore_a_child_while_trashing_its_parent() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    f.execute(&f.plan(std::slice::from_ref(&b), Operation::Trash)?)?;
    let mut restore = proposal(f.trash_path(&b), DependencyPolicy::Include);
    restore.action = RuleAction::Untrash { reason: None };
    let (applied, skipped) = apply_rule_proposals(
        &f.roots,
        &[proposal(a.clone(), DependencyPolicy::Include), restore],
    );
    assert!(applied.is_empty());
    assert_eq!(skipped.len(), 2);
    assert!(skipped
        .iter()
        .all(|s| s.skip_reason.contains("conflicting")));
    assert!(a.exists() && f.trash_path(&b).exists());
    Ok(())
}

#[test]
fn removal_follows_active_descendants_through_a_previously_trashed_intermediate() -> Result<()> {
    let f = Fixture::new()?;
    let a = f.rollout("a", "a", None)?;
    let b = f.rollout("b", "b", Some("a"))?;
    let c = f.rollout("c", "c", Some("b"))?;
    // Simulate an incomplete group left by an older AICS version.
    TrashStore::new(f.roots.trash.clone().unwrap()).trash_file(
        &b,
        Agent::Codex,
        &TrashReason::KeyBinding { key: "old".into() },
    )?;
    let plan = f.plan(std::slice::from_ref(&a), Operation::Trash)?;
    assert_eq!(
        plan.members.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([a, c])
    );
    f.execute(&plan)?;
    assert!(f.trash_path(&b).exists());
    Ok(())
}
