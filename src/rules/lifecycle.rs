use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::{
    AppliedRuleAction, RuleAction, RuleDecision, RulePreviewMatch, RuleProposal, RulesReport,
    SkippedRuleAction,
};
use crate::lifecycle::{self, DependencyGraph, DependencyPolicy, LifecyclePlan, Operation};
use crate::parse::Agent;
use crate::scan::SessionRoots;
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

fn groups(roots: &SessionRoots, proposals: &[RuleProposal]) -> anyhow::Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    for agent in [Agent::Claude, Agent::Codex, Agent::Antigravity] {
        let selected = proposals
            .iter()
            .filter(|p| p.agent == agent && p.included_by.is_none())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let graph = DependencyGraph::discover(
            roots,
            agent,
            &selected.iter().map(|p| p.path.clone()).collect::<Vec<_>>(),
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
    for group in &mut groups {
        let explicit = group
            .triggers
            .iter()
            .map(|p| &p.path)
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
        DependencyGraph::add_live_blockers(&mut group.plan, roots);
    }
    Ok(groups)
}

pub(super) fn expand_preview(roots: &SessionRoots, report: &mut RulesReport) {
    let planned = match groups(roots, &report.proposals) {
        Ok(groups) => groups,
        Err(error) => {
            for proposal in &report.proposals {
                report.skipped.push(skip(proposal, format!("{error:#}")));
            }
            return;
        }
    };
    for group in planned {
        let Some(trigger) = group.triggers.first() else {
            continue;
        };
        let include = group
            .triggers
            .iter()
            .any(|p| p.dependency_policy == DependencyPolicy::Include);
        for (path, member) in &group.plan.members {
            let existing = report
                .proposals
                .iter()
                .any(|p| &p.path == path && operation(p) == group.plan.operation);
            if !existing && include {
                let mut proposal = trigger.clone();
                proposal.path = path.clone();
                proposal.session_id = member.thread_id.clone();
                proposal.included_by = Some(trigger.path.clone());
                report.proposals.push(proposal.clone());
                report.preview_matches.push(RulePreviewMatch {
                    proposals: vec![proposal],
                    decisions: Vec::new(),
                    hit: member.search_hit(),
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
) -> (Vec<AppliedRuleAction>, Vec<SkippedRuleAction>) {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let groups = match groups(roots, proposals) {
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
        for trigger in &group.triggers {
            if let Some(reviewed) = &trigger.reviewed_group {
                if reviewed.members.keys().any(|path| {
                    !proposals
                        .iter()
                        .any(|p| &p.path == path && operation(p) == plan.operation)
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
