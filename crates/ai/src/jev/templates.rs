//! Versioned Jev question templates. Wording matters (a rephrase changed a
//! third of answers in an independent audit), so any edit to instructions
//! or criteria must bump the `*_V` constant, which also invalidates caches.
//!
//! Instructions and criteria are English (research §4.2); picture words
//! match `kadr_analysis::video::bucket_*` so the model reads them as meant.

use super::prefs::Precedent;
use super::service::JevItem;
use crate::providers::jev::JevQuestion;
use kadr_core::Time;
use kadr_project::{DecisionKind, ShotSummary};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const USABILITY_V: &str = "shot_usability/v1";
pub const CAMERA_V: &str = "camera_pick/v1";

/// "4 s", "1 min 20 s".
pub fn duration_words(t: Time) -> String {
    let s = t.as_secs_f64().round() as i64;
    if s < 60 { format!("{s} s") } else { format!("{} min {} s", s / 60, s % 60) }
}

/// The local analyser alone already calls this shot defective.
pub fn shot_is_defective(s: &ShotSummary) -> bool {
    s.black || s.sharpness == "very blurry" || s.exposure == "black" || s.exposure == "blown out" || s.shake == "heavy"
}

/// KEEP / DISCARD / REVIEW for one shot. `speech`: "speech" | "music" | "silence".
pub fn shot_item(subject: String, shot: &ShotSummary, speech: &str) -> JevItem {
    let state = json!({
        "duration": duration_words(shot.range.duration()),
        "sharpness": shot.sharpness,
        "exposure": shot.exposure,
        "shake": shot.shake,
        "audio": speech,
    });
    let usability = JevQuestion::Choice {
        instructions: "Decide what a professional video editor should do with the shot described in `{item}`. \
                       The picture words come from an automatic analyser of focus, exposure and camera shake."
            .into(),
        criteria: BTreeMap::from([
            ("KEEP".into(), json!({"what": "usable footage: sharp, or soft but watchable; normal or dark exposure; no or slight shake",
                                   "not_for": "very blurry, black, blown out or heavily shaking footage"})),
            ("DISCARD".into(), json!({"what": "technically unusable: very blurry, black, blown out, or heavy shake",
                                      "not_for": "footage that is merely boring but technically fine"})),
            ("REVIEW".into(), json!({"what": "mixed or insufficient signals: a human should look at it"})),
        ]),
    };
    let quality = JevQuestion::Score {
        instructions: "Rate the technical picture quality of the shot described in `{item}`.".into(),
        criteria: vec![
            json!("Unusable: very blurry, black or blown out, or heavy shake"),
            json!("Usable with visible flaws: soft focus, dark, or slight shake"),
            json!("Clean: sharp, normal exposure, no shake"),
        ],
    };
    JevItem {
        kind: DecisionKind::ShotUsability,
        subject,
        prompt_version: USABILITY_V,
        features: state.clone(),
        state,
        questions: BTreeMap::from([("usability".into(), usability), ("technical_quality".into(), quality)]),
        primary: "usability".into(),
        extra_ok: shot_is_defective(shot),
    }
}

/// What one camera shows during an interval, in words.
#[derive(Clone, Debug, PartialEq)]
pub struct AngleFeatures {
    pub label: String,
    /// The editor's description of the angle ("wide stage").
    pub description: String,
    pub sharpness: String,
    pub exposure: String,
    pub shake: String,
    /// "loudest" | "quiet" | "silent"
    pub audio_level: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraInterval {
    pub subject: String,
    pub time_label: String,
    pub angles: Vec<AngleFeatures>,
    /// Camera on screen before this interval and for how long.
    pub previous: Option<(String, Time)>,
    /// Extra bucketed context (e.g. {"audio": "music"}).
    pub context: Value,
}

pub const UNSURE: &str = "UNSURE";

/// Camera pick for one interval. With `history`, adds the personal
/// questions `editor_pick` and `has_precedent` (research §5a).
pub fn camera_item(iv: &CameraInterval, history: &[Precedent]) -> JevItem {
    let mut cams = Map::new();
    for a in &iv.angles {
        cams.insert(
            a.label.clone(),
            json!({"shows": a.description, "sharpness": a.sharpness, "exposure": a.exposure, "shake": a.shake, "audio_level": a.audio_level}),
        );
    }
    let features = json!({"context": iv.context, "cameras": cams});
    let mut state = json!({"time": iv.time_label, "context": iv.context, "cameras": cams});
    if let Some((cam, dur)) = &iv.previous {
        state["previous_cut"] = json!(format!("{cam} for {}", duration_words(*dur)));
    }
    let options = |escape: &str| -> BTreeMap<String, Value> {
        let mut m: BTreeMap<String, Value> =
            iv.angles.iter().map(|a| (a.label.clone(), json!({"what": if a.description.is_empty() { a.label.clone() } else { a.description.clone() }}))).collect();
        m.insert(UNSURE.into(), json!(escape));
        m
    };
    let mut questions = BTreeMap::from([(
        "best_cam".to_string(),
        JevQuestion::Choice {
            instructions: "Which camera should a professional multicam editor show during `{item}`? Consider what each camera in \
                           `{item}.cameras` shows, its picture quality and audio level, and `{item}.previous_cut`."
                .into(),
            criteria: options("none of the cameras clearly fits, or the information is insufficient"),
        },
    )]);
    for a in &iv.angles {
        questions.insert(
            format!("fit_{}", a.label),
            JevQuestion::Score {
                instructions: format!("How well does camera `{{item}}.cameras.{}` fit what is happening during `{{item}}`?", a.label),
                criteria: vec![
                    json!("Wrong subject or unusable picture"),
                    json!("Acceptable as a short cutaway"),
                    json!("Good"),
                    json!("Ideal for this moment"),
                ],
            },
        );
    }
    if !history.is_empty() {
        state["editor_history"] = json!(history);
        questions.insert(
            "editor_pick".into(),
            JevQuestion::Choice {
                instructions: "Based on the past decisions in `{item}.editor_history`, which camera will this particular editor choose \
                               for `{item}`?"
                    .into(),
                criteria: options("no similar past situation in `editor_history`"),
            },
        );
        questions.insert(
            "has_precedent".into(),
            JevQuestion::Noul { instructions: "Does `{item}.editor_history` contain situations similar to `{item}`?".into() },
        );
    }
    JevItem {
        kind: DecisionKind::CameraPick,
        subject: iv.subject.clone(),
        prompt_version: CAMERA_V,
        state,
        questions,
        primary: "best_cam".into(),
        features,
        extra_ok: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jev::prefs::Precedent;
    use crate::providers::jev::JevQuestion;
    use kadr_core::{Time, TimeRange};
    use kadr_project::ShotSummary;
    use serde_json::json;

    fn shot(sharp: &str, exposure: &str, shake: &str) -> ShotSummary {
        ShotSummary {
            range: TimeRange::new(Time::ZERO, Time::from_secs(4)),
            sharpness: sharp.into(),
            exposure: exposure.into(),
            shake: shake.into(),
            black: exposure == "black",
        }
    }

    fn angle(label: &str) -> AngleFeatures {
        AngleFeatures {
            label: label.into(),
            description: format!("{label} view"),
            sharpness: "sharp".into(),
            exposure: "normal".into(),
            shake: "none".into(),
            audio_level: "quiet".into(),
        }
    }

    fn interval(labels: &[&str]) -> CameraInterval {
        CameraInterval {
            subject: "clip:x@0".into(),
            time_label: "00:12-00:16".into(),
            angles: labels.iter().map(|l| angle(l)).collect(),
            previous: Some(("CAM1".into(), Time::from_secs(14))),
            context: json!({"audio": "music"}),
        }
    }

    #[test]
    fn shot_item_asks_usability_with_escape_and_flags_defects() {
        let it = shot_item("asset:a:shot:0".into(), &shot("very blurry", "normal", "heavy"), "music");
        let JevQuestion::Choice { criteria, instructions } = &it.questions[&it.primary] else { panic!() };
        assert!(criteria.contains_key("REVIEW") && criteria.contains_key("KEEP") && criteria.contains_key("DISCARD"));
        assert!(instructions.contains("{item}"));
        assert!(it.extra_ok, "the local detector sees a defect");
        assert_eq!(it.state["sharpness"], "very blurry");
        assert!(!shot_item("s".into(), &shot("sharp", "normal", "none"), "speech").extra_ok);
        let JevQuestion::Score { criteria, .. } = &it.questions["technical_quality"] else { panic!() };
        assert_eq!(criteria.len(), 3);
    }

    #[test]
    fn camera_item_has_escape_option_scores_and_history() {
        let prec = Precedent { context: json!({"audio": "music"}), ai_suggested: "CAM2".into(), editor_chose: "CAM1".into(), at_ms: 0 };
        let it = camera_item(&interval(&["CAM1", "CAM2"]), &[prec]);
        assert_eq!(it.primary, "best_cam");
        let JevQuestion::Choice { criteria, .. } = &it.questions["best_cam"] else { panic!() };
        assert!(criteria.contains_key("UNSURE") && criteria.contains_key("CAM2"));
        assert!(it.questions.contains_key("fit_CAM1") && it.questions.contains_key("fit_CAM2"));
        assert!(it.questions.contains_key("editor_pick") && it.questions.contains_key("has_precedent"));
        assert_eq!(it.state["editor_history"].as_array().unwrap().len(), 1);
        assert_eq!(it.state["previous_cut"], "CAM1 for 14 s");
        assert!(!it.state.to_string().contains(":\\\\"), "no file paths");

        let plain = camera_item(&interval(&["CAM1", "CAM2"]), &[]);
        assert!(!plain.questions.contains_key("editor_pick"), "no history, no personal question");
        assert!(plain.state.get("editor_history").is_none());
    }
}
