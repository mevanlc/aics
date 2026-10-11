use super::*;
use crate::lifecycle::{self, LifecyclePlan, Operation};
use crate::tui::dependencies::{Outcome, Review};

impl App {
    pub(super) fn start_lifecycle(
        &mut self,
        hit: SearchHit,
        operation: Operation,
        reason: TrashReason,
        confirm: bool,
    ) -> Result<()> {
        let review = Review::new(
            &self.roots,
            hit.session.agent,
            hit.session.file_path,
            operation,
            reason,
            true,
        )?;
        if confirm || review.plan.expanded() || !review.plan.blockers.is_empty() {
            self.overlay = Overlay::Dependencies(review);
            Ok(())
        } else {
            self.execute_lifecycle(&review.plan, &review.reason)
        }
    }

    pub(super) fn review_selected_dependencies(&mut self) -> Result<()> {
        let Some(hit) = self.selected_hit() else {
            return Ok(());
        };
        self.overlay = Overlay::Dependencies(Review::new(
            &self.roots,
            hit.session.agent,
            hit.session.file_path,
            if hit.session.trashed {
                Operation::Restore
            } else {
                Operation::Trash
            },
            TrashReason::ActionMenu {
                action: "Related sessions".into(),
            },
            false,
        )?);
        Ok(())
    }

    pub(super) fn handle_dependency_key(&mut self, key: KeyEvent) -> Result<()> {
        let Overlay::Dependencies(mut review) = std::mem::replace(&mut self.overlay, Overlay::None)
        else {
            return Ok(());
        };
        match review.key(key) {
            Outcome::Stay => self.overlay = Overlay::Dependencies(review),
            Outcome::Close => {}
            Outcome::View(member) => {
                let hit = member.search_hit();
                let session = match crate::parse::parse_session_file(member.agent, &member.path) {
                    Ok(session) => session,
                    Err(error) => {
                        review.notice = format!("Cannot open transcript: {error:#}");
                        self.overlay = Overlay::Dependencies(review);
                        return Ok(());
                    }
                };
                self.preview_cache.insert(member.path, session);
                self.viewer_hit = Some(hit);
                self.dependency_before_viewer = Some(review);
                self.overlay = Overlay::Viewer(ViewerState::with_search(""));
            }
            Outcome::Execute => match review.refresh() {
                Ok(true) => {
                    review.notice = "Group changed; review the updated selection.".into();
                    self.overlay = Overlay::Dependencies(review);
                }
                Ok(false) => {
                    if let Err(error) = self.execute_lifecycle(&review.plan, &review.reason) {
                        review.notice = format!("{error:#}");
                        let _ = review.refresh();
                        self.overlay = Overlay::Dependencies(review);
                    }
                }
                Err(error) => {
                    review.notice = format!("Cannot recheck group: {error:#}");
                    self.overlay = Overlay::Dependencies(review);
                }
            },
        }
        Ok(())
    }

    pub(super) fn execute_lifecycle(
        &mut self,
        plan: &LifecyclePlan,
        reason: &TrashReason,
    ) -> Result<()> {
        let reasons = plan
            .members
            .keys()
            .map(|p| (p.clone(), lifecycle::group_reason(reason, plan, p)))
            .collect();
        lifecycle::execute(&self.roots, plan, &reasons)?;
        self.results
            .retain(|hit| !plan.members.contains_key(&hit.session.file_path));
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
        for path in plan.members.keys() {
            self.preview_cache.remove(path);
            self.hidden_deleted_paths.insert(path.clone());
        }
        self.overlay = Overlay::None;
        self.dependency_before_viewer = None;
        self.viewer_hit = None;
        self.release_closed_viewer();
        self.preview_render_cache = None;
        self.cancel_pending_searches();
        if let Err(error) = self.manager.sync_with_roots_best_effort(&self.roots, false) {
            self.statusline = Some(statusline::Entry::failed(format!(
                "{} completed; index refresh failed: {error:#}",
                plan.operation.label()
            )));
            return Ok(());
        }
        self.statusline = Some(statusline::Entry::completed(format!(
            "{} completed: {} sessions ({} files)",
            plan.operation.label(),
            plan.session_count(),
            plan.members.len()
        )));
        Ok(())
    }
}
