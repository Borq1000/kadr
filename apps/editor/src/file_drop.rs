//! Files dragged in from Explorer. Slint has no event for OS file drops, but
//! its winit backend lets us see raw window events: winit reports one
//! `HoveredFile`/`DroppedFile` per file, so drops are collected and handled
//! as a single batch on the next event-loop turn.

use crate::app::{with_app, App};
use crate::import::MEDIA_EXTENSIONS;
use crate::AppWindow;
use kadr_i18n::{t, tn};
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Folders are scanned this deep, so dropping a card dump works but dropping
/// `C:\` doesn't walk the whole disk.
const MAX_DEPTH: usize = 3;
const MAX_FILES: usize = 2000;

#[derive(Default)]
struct DropState {
    hovered: usize,
    dropped: Vec<PathBuf>,
}

pub fn install(ui: &AppWindow) {
    let state = Rc::new(RefCell::new(DropState::default()));
    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |_, event| {
        let Some(ui) = weak.upgrade() else { return EventResult::Propagate };
        match event {
            WindowEvent::HoveredFile(_) => {
                let mut s = state.borrow_mut();
                s.hovered += 1;
                ui.set_file_drop_label(tn("drop.title", s.hovered as i64, &[]).into());
                ui.set_file_drop_active(true);
            }
            WindowEvent::HoveredFileCancelled => {
                state.borrow_mut().hovered = 0;
                ui.set_file_drop_active(false);
            }
            WindowEvent::DroppedFile(path) => {
                let mut s = state.borrow_mut();
                s.hovered = 0;
                if s.dropped.is_empty() {
                    let state = state.clone();
                    // All DroppedFile events of one drop arrive back to back.
                    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                        let paths = std::mem::take(&mut state.borrow_mut().dropped);
                        with_app(|app| app.on_files_dropped(paths));
                    });
                }
                s.dropped.push(path.clone());
                ui.set_file_drop_active(false);
            }
            _ => return EventResult::Propagate,
        }
        EventResult::PreventDefault
    });
}

/// What a drop turns into.
#[derive(Debug, Default, PartialEq)]
pub struct DropPlan {
    pub project: Option<PathBuf>,
    pub media: Vec<PathBuf>,
    pub skipped: usize,
}

fn is_media(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| MEDIA_EXTENSIONS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

fn is_project(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(kadr_project::io::EXTENSION))
}

/// Sorts dropped paths: the first `.kadr` becomes the project to open, media
/// files (also found inside dropped folders) are imported, the rest is
/// counted so the user learns why nothing happened.
pub fn plan_drop(paths: Vec<PathBuf>) -> DropPlan {
    fn walk(dir: &Path, depth: usize, plan: &mut DropPlan) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for p in entries {
            if plan.media.len() >= MAX_FILES {
                return;
            }
            if p.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&p, depth + 1, plan);
                }
            } else if is_media(&p) {
                plan.media.push(p);
            }
        }
    }
    let mut plan = DropPlan::default();
    for p in paths {
        if p.is_dir() {
            walk(&p, 1, &mut plan);
        } else if is_project(&p) {
            if plan.project.is_none() {
                plan.project = Some(p);
            } else {
                plan.skipped += 1;
            }
        } else if is_media(&p) {
            plan.media.push(p);
        } else {
            plan.skipped += 1;
        }
    }
    plan
}

impl App {
    pub fn on_files_dropped(&mut self, paths: Vec<PathBuf>) {
        let plan = plan_drop(paths);
        tracing::info!(project = ?plan.project, media = plan.media.len(), skipped = plan.skipped, "files dropped");
        if plan.skipped > 0 {
            self.toast_warn(tn("toast.drop_skipped", plan.skipped as i64, &[]));
        }
        match plan.project {
            // Opening replaces the current project, so dropped media would be
            // lost; a project drop wins and media is ignored.
            Some(p) => {
                if !plan.media.is_empty() {
                    self.toast_warn(t("toast.drop_project_only"));
                }
                let after_discard = p.clone();
                if !self.guard_unsaved(Box::new(move |app| app.open_project(after_discard))) {
                    self.open_project(p);
                }
            }
            None if plan.media.is_empty() => {
                if plan.skipped == 0 {
                    self.toast_warn(t("toast.drop_nothing"));
                }
            }
            None => {
                self.ui().set_welcome_open(false);
                self.import_paths(plan.media);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_scanned_and_unknown_files_counted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("card/DCIM")).unwrap();
        for f in ["card/DCIM/a.MP4", "card/DCIM/b.wav", "card/notes.txt", "c.mov"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let plan = plan_drop(vec![root.join("card"), root.join("c.mov"), root.join("readme.pdf")]);
        assert_eq!(plan.project, None);
        assert_eq!(plan.media, vec![root.join("card/DCIM/a.MP4"), root.join("card/DCIM/b.wav"), root.join("c.mov")]);
        // Inside folders non-media is silently ignored; a dropped file is not.
        assert_eq!(plan.skipped, 1);
    }

    #[test]
    fn first_project_wins() {
        let plan = plan_drop(vec![PathBuf::from("x.KADR"), PathBuf::from("y.kadr"), PathBuf::from("clip.mp4")]);
        assert_eq!(plan.project, Some(PathBuf::from("x.KADR")));
        assert_eq!(plan.skipped, 1);
        assert_eq!(plan.media, vec![PathBuf::from("clip.mp4")]);
    }
}
