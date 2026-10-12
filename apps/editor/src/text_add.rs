//! Adding a text (titles spec §1, §7): «Т» on the timeline, Ctrl+T, Edit →
//! «Добавить текст», the «Текст» tab. A 5 s title at the playhead (or a
//! drop point) on the topmost video track when free there, else on a new
//! track above (`EditCommand::AddText`), one undo entry; the clip is
//! selected and inline typing starts (`text_added`, Task 8).

use crate::app::App;
use kadr_core::{ClipId, Time, TrackId};
use kadr_i18n::t;
use kadr_project::{Clip, Sequence, TextDoc};
use kadr_timeline::EditCommand;

/// A 5 s text clip of the starter style for `seq`, at `at` snapped to a frame (never before 0).
pub fn new_text_clip(seq: &Sequence, at: Time, text: &str) -> Clip {
    Clip::new_text(TextDoc::starter(text, seq.height), seq.frame_rate.snap(at.max(Time::ZERO)), Time::from_secs(5))
}

impl App {
    pub fn add_text_at_playhead(&mut self) {
        self.add_text(self.playhead, None, None, None);
    }

    /// Adds a text clip at `at` (`doc`: a style's; `None`: the starter) on
    /// `track` (`None`: the command chooses), its block centred at `pos`
    /// (`Transform.x / y`; `None`: the frame's centre). Returns its id.
    pub fn add_text(&mut self, at: Time, track: Option<TrackId>, doc: Option<TextDoc>, pos: Option<(f64, f64)>) -> Option<ClipId> {
        let seq = self.project.sequence();
        let mut clip = new_text_clip(seq, at, &t("text.default"));
        if let Some(d) = doc {
            clip.name = d.label();
            clip.text = Some(Box::new(d));
        }
        if let Some((x, y)) = pos {
            (clip.transform.x, clip.transform.y) = (x, y);
        }
        let id = clip.id;
        if !self.execute(EditCommand::AddText { clip, track }) {
            return None;
        }
        self.tl.selection = vec![id];
        self.refresh_timeline();
        self.refresh_inspector();
        self.refresh_status();
        self.text_added(id);
        Some(id)
    }

    /// After a text was added (Task 8: starts inline typing with the text selected).
    pub fn text_added(&mut self, _id: ClipId) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::FrameRate;
    use kadr_project::Project;

    #[test]
    fn a_new_text_is_five_seconds_of_the_starter_at_a_frame() {
        let mut p = Project::new("t");
        (p.sequence_mut().frame_rate, p.sequence_mut().height) = (FrameRate::FPS_25, 720);
        let c = new_text_clip(p.sequence(), Time::from_millis(1_013), "Ваш текст");
        assert_eq!((c.timeline_in, c.duration()), (Time::from_millis(1_000), Time::from_secs(5)), "snapped to the 25 fps grid");
        let doc = c.text.as_deref().unwrap();
        assert_eq!((doc.plain(), doc.style.size), ("Ваш текст".to_string(), 65.0), "9 % of 720 lines");
        assert_eq!(new_text_clip(p.sequence(), Time::from_secs(-3), "x").timeline_in, Time::ZERO);
    }
}
