use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use super::{
    AppliedRuleAction, RuleAction, RuleDecision, RulePreviewMatch, RuleProposal, RulesReport,
    SkippedRuleAction,
};
use crate::lifecycle::{self, DependencyGraph, DependencyPolicy, LifecyclePlan, Operation};
use crate::live::LiveSessionTracker;
use crate::parse::Agent;
use crate::scan::{SessionFile, SessionRoots};
use crate::trash::TrashReason;

pub(super) fn serialize_review<S: serde::Serializer>(
    plan: &Option<LifecyclePlan>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::Serialize;
    plan.as_ref().map(|p| serde_json::json!({
        "sessions": p.session_count(), "files": p.members.len(), "blockers": p.blockers,
        "members": p.members.keys().map(|path| serde_json::json!({
            "path": path, "selected": p.seeds.contains(path), "relation": p.reasons.get(path)
        })).collect::<Vec<_>>()
    })).serialize(serializer)
}

fn operation(proposal: &RuleProposal) -> Operation {
    match proposal.action {
        RuleAction::Trash { .. } => Operation::Trash,
        RuleAction::Untrash { .. } => Operation::Restore,
    }
}

struct Group {
    plan: LifecyclePlan,
    triggers: Vec<RuleProposal>,
}

/// Discovery reads every session in a provider home, so one rules run plans its preview
/// expansion and its application from the same graphs. `lifecycle::execute` still
/// rediscovers under the operation lock before anything moves.
#[derive(Default)]
pub(super) struct DependencyGraphs {
    graphs: Vec<(Agent, Vec<PathBuf>, DependencyGraph)>,
}

impl DependencyGraphs {
    fn get(
        &mut self,
        roots: &SessionRoots,
        agent: Agent,
        seeds: Vec<PathBuf>,
    ) -> anyhow::Result<&DependencyGraph> {
        let index = match self
            .graphs
            .iter()
            .position(|(a, s, _)| *a == agent && *s == seeds)
        {
            Some(index) => index,
            None => {
                let start = std::time::Instant::now();
                let graph = DependencyGraph::discover(roots, agent, &seeds)?;
                log::debug!(
                    "rules: discovered {agent:?} dependency graph ({} members, {} errors) in {:?}",
                    graph.members.len(),
                    graph.errors.len(),
                    start.elapsed()
                );
                self.graphs.push((agent, seeds, graph));
                self.graphs.len() - 1
            }
        };
        Ok(&self.graphs[index].2)
    }
}

fn groups(
    roots: &SessionRoots,
    proposals: &[RuleProposal],
    graphs: &mut DependencyGraphs,
) -> anyhow::Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let selected = proposals
            .iter()
            .filter(|p| p.agent == agent && p.included_by.is_none())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let graph = graphs.get(
            roots,
            agent,
            selected.iter().map(|p| p.path.clone()).collect(),
        )?;
        for proposal in selected {
            let mut seeds = BTreeSet::from([proposal.path.clone()]);
            let mut triggers = vec![proposal.clone()];
            loop {
                let plan = graph.plan(seeds.clone(), operation(proposal));
                let overlapping = groups.iter().position(|g| {
                    g.plan.operation == plan.operation
                        && g.plan.members.keys().any(|p| plan.members.contains_key(p))
                });
                if let Some(index) = overlapping {
                    let other = groups.remove(index);
                    seeds.extend(other.plan.seeds);
                    triggers.extend(other.triggers);
                } else {
                    groups.push(Group { plan, triggers });
                    break;
                }
            }
        }
    }
    // Conflicting operations cannot be made safe by reordering them.
    for i in 0..groups.len() {
        for j in i + 1..groups.len() {
            if groups[i].plan.operation != groups[j].plan.operation
                && groups[i]
                    .plan
                    .members
                    .keys()
                    .any(|p| groups[j].plan.members.contains_key(p))
                || (groups[i].plan.operation != groups[j].plan.operation
                    && groups[i].plan.members.values().any(|a| {
                        groups[j]
                            .plan
                            .members
                            .values()
                            .any(|b| a.requires(b) || b.requires(a) || a.same_thread(b))
                    }))
            {
                groups[i]
                    .plan
                    .blockers
                    .push("conflicting trash and restore proposals in this group".into());
                groups[j]
                    .plan
                    .blockers
                    .push("conflicting trash and restore proposals in this group".into());
            }
        }
    }
    let live = LiveSessionTracker::new(roots.live_sessions.clone()).snapshot();
    for group in &mut groups {
        let explicit = group
            .triggers
            .iter()
            .flat_map(|p| {
                std::iter::once(&p.path)
                    .chain(p.source.iter().flat_map(|source| source.owned_paths.iter()))
            })
            .collect::<BTreeSet<_>>();
        if !group
            .triggers
            .iter()
            .any(|p| p.dependency_policy == DependencyPolicy::Include)
            && group.plan.members.keys().any(|p| !explicit.contains(p))
        {
            group
                .plan
                .blockers
                .push("dependencyPolicy=skip: additional group members are required".into());
        }
        DependencyGraph::add_live_blockers_from(&mut group.plan, &live);
    }
    Ok(groups)
}

pub(super) fn expand_preview(
    roots: &SessionRoots,
    files: &[SessionFile],
    report: &mut RulesReport,
    graphs: &mut DependencyGraphs,
) {
    let owners = files
        .iter()
        .flat_map(|file| {
            file.conversation.iter().flat_map(move |source| {
                source
                    .owned_paths
                    .iter()
                    .map(move |path| (path.clone(), file))
            })
        })
        .collect::<HashMap<_, _>>();
    let planned = match groups(roots, &report.proposals, graphs) {
        Ok(groups) => groups,
        Err(error) => {
            for proposal in &report.proposals {
                report.skipped.push(skip(proposal, format!("{error:#}")));
            }
            return;
        }
    };
    for mut group in planned {
        for skip in &report.processing_skips {
            if group.plan.members.contains_key(&skip.path) {
                group.plan.blockers.push(format!(
                    "required conversation cannot be evaluated: {}",
                    skip.reason
                ));
            }
        }
        let Some(trigger) = group.triggers.first() else {
            continue;
        };
        let include = group
            .triggers
            .iter()
            .any(|p| p.dependency_policy == DependencyPolicy::Include);
        for (path, member) in &group.plan.members {
            let logical = owners.get(path).copied();
            let proposal_path = logical.map_or(path, |file| &file.path);
            let existing = report
                .proposals
                .iter()
                .any(|p| &p.path == proposal_path && operation(p) == group.plan.operation);
            let owned_page = group.triggers.iter().any(|trigger| {
                trigger
                    .source
                    .as_ref()
                    .is_some_and(|source| source.owned_paths.contains(path))
            });
            if !existing && include && !owned_page {
                let mut proposal = trigger.clone();
                proposal.path = proposal_path.clone();
                proposal.session_id = member.thread_id.clone();
                proposal.included_by = Some(trigger.path.clone());
                proposal.source = logical.and_then(|file| file.conversation.clone());
                report.proposals.push(proposal.clone());
                report.preview_matches.push(RulePreviewMatch {
                    proposals: vec![proposal],
                    decisions: Vec::new(),
                    hit: logical
                        .and_then(|file| {
                            crate::parse::parse_scanned_session_file(file)
                                .ok()
                                .flatten()
                                .map(|session| {
                                    super::rule_search_hit(super::stored_rule_session(
                                        &session, file, None,
                                    ))
                                })
                        })
                        .unwrap_or_else(|| member.search_hit()),
                });
            }
        }
        let explanation = if group.plan.blockers.is_empty() {
            format!(
                "{} group: {} sessions, {} files",
                group.plan.operation.label(),
                group.plan.session_count(),
                group.plan.members.len()
            )
        } else {
            format!("Blocked: {}", group.plan.blockers.join("; "))
        };
        for proposal in &mut report.proposals {
            if group.plan.members.contains_key(&proposal.path)
                && operation(proposal) == group.plan.operation
            {
                proposal.reviewed_group = Some(group.plan.clone());
            }
        }
        for matched in &mut report.preview_matches {
            if group
                .plan
                .members
                .contains_key(&matched.hit.session.file_path)
            {
                for p in &mut matched.proposals {
                    if operation(p) == group.plan.operation {
                        p.reviewed_group = Some(group.plan.clone());
                    }
                }
                let relation = group
                    .plan
                    .reasons
                    .get(&matched.hit.session.file_path)
                    .cloned()
                    .unwrap_or_default();
                matched.decisions.push(RuleDecision {
                    rule: trigger.rule.clone(),
                    action: "dependencies".into(),
                    text: format!("{explanation} · {relation}"),
                });
            }
        }
    }
}

fn skip(p: &RuleProposal, reason: String) -> SkippedRuleAction {
    SkippedRuleAction {
        rule: p.rule.clone(),
        session_id: p.session_id.clone(),
        path: p.path.clone(),
        agent: p.agent,
        action: p.action.clone(),
        skip_reason: reason,
    }
}

pub(super) fn apply(
    roots: &SessionRoots,
    proposals: &[RuleProposal],
    graphs: &mut DependencyGraphs,
) -> (Vec<AppliedRuleAction>, Vec<SkippedRuleAction>) {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let groups = match groups(roots, proposals, graphs) {
        Ok(groups) => groups,
        Err(error) => {
            return (
                applied,
                proposals
                    .iter()
                    .map(|p| skip(p, format!("{error:#}")))
                    .collect(),
            )
        }
    };
    for group in groups {
        let mut plan = group.plan;
        for proposal in proposals.iter().filter(|proposal| {
            plan.members.contains_key(&proposal.path) && operation(proposal) == plan.operation
        }) {
            if proposal
                .source
                .iter()
                .chain(proposal.keeper_source.iter())
                .any(|source| !source.is_current())
            {
                plan.blockers.push(
                    "conversation history changed since rule evaluation; refresh the preview"
                        .into(),
                );
            }
            if let Some(reviewed) = &proposal.reviewed_group {
                plan.blockers.extend(reviewed.blockers.iter().cloned());
            }
        }
        for trigger in &group.triggers {
            if let Some(reviewed) = &trigger.reviewed_group {
                if reviewed.members.keys().any(|path| {
                    !proposals.iter().any(|p| {
                        operation(p) == plan.operation
                            && (&p.path == path
                                || p.source
                                    .as_ref()
                                    .is_some_and(|source| source.owned_paths.contains(path)))
                    })
                }) {
                    plan.blockers.push(
                        "required members were unmarked; select the complete reviewed group".into(),
                    );
                }
                if reviewed.members != plan.members {
                    plan.blockers
                        .push("session group changed since preview; refresh the preview".into());
                }
            }
        }
        let provenance = group
            .triggers
            .iter()
            .map(|p| TrashReason::Rule {
                name: p.rule.clone(),
                reason: match &p.action {
                    RuleAction::Trash { reason } | RuleAction::Untrash { reason } => reason.clone(),
                },
            })
            .collect::<Vec<_>>();
        let reasons = plan
            .members
            .keys()
            .map(|path| {
                let reason = if provenance.len() == 1 && plan.members.len() == 1 {
                    provenance[0].clone()
                } else {
                    TrashReason::DependencyGroup {
                        triggers: provenance.clone(),
                        selected_paths: plan
                            .seeds
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect(),
                        relation: plan.reasons.get(path).cloned().unwrap_or_default(),
                    }
                };
                (path.clone(), reason)
            })
            .collect::<BTreeMap<PathBuf, _>>();
        match lifecycle::execute(roots, &plan, &reasons) {
            Ok(()) => {
                for member in plan.members.values() {
                    let trigger = group
                        .triggers
                        .iter()
                        .find(|p| p.path == member.path)
                        .unwrap_or(&group.triggers[0]);
                    applied.push(AppliedRuleAction {
                        rule: trigger.rule.clone(),
                        triggers: if provenance.len() > 1 {
                            provenance.clone()
                        } else {
                            Vec::new()
                        },
                        session_id: member.thread_id.clone(),
                        path: member.path.clone(),
                        agent: member.agent,
                        action: trigger.action.clone(),
                    });
                }
            }
            Err(error) => {
                for trigger in &group.triggers {
                    skipped.push(skip(trigger, format!("{error:#}")));
                }
            }
        }
    }
    // Expanded preview rows must never become independent authorization when their trigger is unmarked.
    for p in proposals.iter().filter(|p| p.included_by.is_some()) {
        if !applied.iter().any(|a| a.path == p.path) {
            skipped.push(skip(
                p,
                "required group was not applied; keep its triggering proposal selected".into(),
            ));
        }
    }
    (applied, skipped)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn dependency_graphs_discover_each_agent_and_seed_set_once() {
        let temp = TempDir::new().unwrap();
        let roots = SessionRoots {
            live_sessions: Default::default(),
            claude_projects: temp.path().join("claude"),
            codex_sessions: temp.path().join("codex/sessions"),
            antigravity_home: temp.path().join("antigravity"),
            trash: None,
        };
        fs::create_dir_all(&roots.codex_sessions).unwrap();
        let session = roots.codex_sessions.join("one.jsonl");
        fs::write(
            &session,
            r#"{"type":"session_meta","payload":{"id":"one"}}"#,
        )
        .unwrap();
        let mut graphs = DependencyGraphs::default();

        let first = graphs
            .get(&roots, Agent::Codex, vec![session.clone()])
            .unwrap()
            .members
            .len();
        // A file added after discovery stays invisible until the next rules run.
        fs::write(
            roots.codex_sessions.join("two.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"two"}}"#,
        )
        .unwrap();
        let reused = graphs
            .get(&roots, Agent::Codex, vec![session.clone()])
            .unwrap()
            .members
            .len();
        let fresh = graphs
            .get(&roots, Agent::Codex, Vec::new())
            .unwrap()
            .members
            .len();

        assert_eq!((first, reused, fresh), (1, 1, 2));
        assert_eq!(graphs.graphs.len(), 2);
    }
}
