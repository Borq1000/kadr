//! Edit engine with generic undo/redo.
//!
//! Instead of hand-writing an inverse for every command, the engine snapshots
//! the sequence before a command, applies it, and stores only the parts that
//! changed (per track, plus markers/transitions/track layout). Undo swaps
//! the "before" parts back in. This makes every command — including
//! multi-track ripples and 50-operation AI batches — reversible by
//! construction, at a memory cost proportional to what actually changed.

use crate::commands::{EditCommand, EditContext, EditError};
use kadr_core::{ActionId, SequenceId, TrackId};
use kadr_project::{EditOperation, EditSource, Marker, Project, Track, Transition, now_ms};

#[derive(Clone, Debug)]
struct SeqState {
    /// Changed tracks (full content), keyed by id.
    tracks: Vec<Track>,
    /// Full track order if tracks were added/removed/reordered.
    order: Option<Vec<TrackId>>,
    markers: Option<Vec<Marker>>,
    transitions: Option<Vec<Transition>>,
}

#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// Unique, monotonically assigned id (for "is this the saved state?").
    pub id: u64,
    pub label: String,
    pub source: EditSource,
    /// Set for AI edits so the UI can offer "Undo AI edit".
    pub action: Option<ActionId>,
    sequence: SequenceId,
    before: SeqState,
    after: SeqState,
}

pub struct EditEngine {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    limit: usize,
    /// Incremented on every change — cheap "dirty"/"changed" detection for UI.
    revision: u64,
    next_id: u64,
}

impl Default for EditEngine {
    fn default() -> Self {
        EditEngine { undo: vec![], redo: vec![], limit: 500, revision: 0, next_id: 1 }
    }
}

impl EditEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Identifies the current point in history. Undoing back to a saved
    /// state yields the same marker again, so the document is clean.
    pub fn history_marker(&self) -> u64 {
        self.undo.last().map_or(0, |e| e.id)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }
    pub fn last_entry(&self) -> Option<&HistoryEntry> {
        self.undo.last()
    }
    /// AI action of the most recently undone edit, if any.
    pub fn redo_top_action(&self) -> Option<ActionId> {
        self.redo.last().and_then(|e| e.action)
    }

    pub fn execute(&mut self, project: &mut Project, cmd: EditCommand) -> Result<(), EditError> {
        self.execute_as(project, cmd, EditSource::User, None)
    }

    /// Applies a command atomically: on error the sequence is left untouched.
    pub fn execute_as(
        &mut self,
        project: &mut Project,
        cmd: EditCommand,
        source: EditSource,
        action: Option<ActionId>,
    ) -> Result<(), EditError> {
        let label = cmd.label();
        let seq_id = project.active_sequence;
        let before = project.sequence().clone();
        let result = {
            let (assets, multicam, seq) = split_borrow(project);
            cmd.apply(seq, &EditContext { assets, multicam })
        };
        if let Err(e) = result {
            *project.sequence_mut() = before;
            tracing::debug!(%label, error = %e, "edit rejected");
            return Err(e);
        }
        let after = project.sequence();
        debug_assert!(after.tracks.iter().all(Track::is_sorted_and_disjoint), "track invariant broken by {label}");

        let (b, a) = diff(&before, after);
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(HistoryEntry { id, label: label.clone(), source, action, sequence: seq_id, before: b, after: a });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision += 1;
        project.history_log.push(EditOperation { at_ms: now_ms(), source, label, action });
        Ok(())
    }

    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let e = self.undo.pop()?;
        restore(project, e.sequence, &e.before);
        let label = e.label.clone();
        self.redo.push(e);
        self.revision += 1;
        Some(label)
    }

    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let e = self.redo.pop()?;
        restore(project, e.sequence, &e.after);
        let label = e.label.clone();
        self.undo.push(e);
        self.revision += 1;
        Some(label)
    }

    /// Undoes the most recent edit belonging to `action` — only if it is the
    /// top of the stack (undoing out of order could corrupt later edits).
    pub fn undo_action(&mut self, project: &mut Project, action: ActionId) -> Option<String> {
        if self.undo.last().and_then(|e| e.action) == Some(action) { self.undo(project) } else { None }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.revision += 1;
    }
}

fn split_borrow(p: &mut Project) -> (&[kadr_project::MediaAsset], &[kadr_project::MulticamGroup], &mut kadr_project::Sequence) {
    let id = p.active_sequence;
    let idx = p.sequences.iter().position(|s| s.id == id).unwrap_or(0);
    (&p.assets, &p.multicam_groups, &mut p.sequences[idx])
}

fn diff(before: &kadr_project::Sequence, after: &kadr_project::Sequence) -> (SeqState, SeqState) {
    let order_b: Vec<TrackId> = before.tracks.iter().map(|t| t.id).collect();
    let order_a: Vec<TrackId> = after.tracks.iter().map(|t| t.id).collect();
    let layout_changed = order_b != order_a;
    let mut tb = vec![];
    let mut ta = vec![];
    for t in &before.tracks {
        match after.track(t.id) {
            Some(n) if n == t => {}
            _ => tb.push(t.clone()),
        }
    }
    for t in &after.tracks {
        match before.track(t.id) {
            Some(o) if o == t => {}
            _ => ta.push(t.clone()),
        }
    }
    let markers = before.markers != after.markers;
    let transitions = before.transitions != after.transitions;
    (
        SeqState {
            tracks: tb,
            order: layout_changed.then_some(order_b),
            markers: markers.then(|| before.markers.clone()),
            transitions: transitions.then(|| before.transitions.clone()),
        },
        SeqState {
            tracks: ta,
            order: layout_changed.then_some(order_a),
            markers: markers.then(|| after.markers.clone()),
            transitions: transitions.then(|| after.transitions.clone()),
        },
    )
}

fn restore(project: &mut Project, seq_id: SequenceId, state: &SeqState) {
    let Some(seq) = project.sequences.iter_mut().find(|s| s.id == seq_id) else {
        return;
    };
    if let Some(order) = &state.order {
        // Rebuild layout: take tracks from the state first, else current.
        let mut current: Vec<Track> = std::mem::take(&mut seq.tracks);
        seq.tracks = order
            .iter()
            .filter_map(|id| {
                state.tracks.iter().find(|t| t.id == *id).cloned().or_else(|| {
                    current.iter().position(|t| t.id == *id).map(|i| current.swap_remove(i))
                })
            })
            .collect();
    } else {
        for t in &state.tracks {
            if let Some(slot) = seq.track_mut(t.id) {
                *slot = t.clone();
            }
        }
    }
    if let Some(m) = &state.markers {
        seq.markers = m.clone();
    }
    if let Some(t) = &state.transitions {
        seq.transitions = t.clone();
    }
}
