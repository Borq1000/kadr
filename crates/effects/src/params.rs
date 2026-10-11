//! Parameter definitions of recipes (effects spec §2, §5): validation for
//! commands, defaults for new effects, and the values a `compile` sees.

use kadr_project::{MAX_KEY_FLICKS, ParamValue, SourceTime};
use std::collections::BTreeMap;

/// The «Обычно» strength (spec §2): the preset as designed.
pub const NORMAL: f64 = 0.6;

/// Intensity multiplier `k = s / 0.6`: 0 % → none, 60 % → 1, 100 % → 1.67.
pub fn intensity(strength: f64) -> f64 {
    strength / NORMAL
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    /// A number in `min..=max`; only numbers can be animated (`fx:` tracks).
    Number { min: f64, max: f64, default: f64 },
    /// One of `options`.
    Choice { options: &'static [&'static str], default: &'static str },
    /// A point of the clip's picture, fractions 0..1; default the centre.
    Point,
    /// A source moment; default: the source frame where the effect is applied.
    Moment,
}

impl ParamKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            ParamKind::Number { .. } => "number",
            ParamKind::Choice { .. } => "choice",
            ParamKind::Point => "point",
            ParamKind::Moment => "moment",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamDef {
    pub key: &'static str,
    pub kind: ParamKind,
}

impl ParamDef {
    /// i18n key of the parameter's label: `fx.param.<key>`.
    pub fn label_key(&self) -> String {
        format!("fx.param.{}", self.key)
    }

    pub fn animatable(&self) -> bool {
        matches!(self.kind, ParamKind::Number { .. })
    }

    /// Whether `v` is a valid value of this parameter; the error names the
    /// key and the reason (shown to MCP / AI clients).
    pub fn check(&self, v: &ParamValue) -> Result<(), String> {
        let k = self.key;
        match (self.kind, v) {
            (ParamKind::Number { min, max, .. }, ParamValue::Number(n)) => {
                if n.is_finite() && (min..=max).contains(n) { Ok(()) } else { Err(format!("{k} must be {min}..{max}, got {n}")) }
            }
            (ParamKind::Choice { options, .. }, ParamValue::Choice(c)) => {
                if options.contains(&c.as_str()) { Ok(()) } else { Err(format!("{k} must be one of {}, got {c:?}", options.join(", "))) }
            }
            (ParamKind::Point, ParamValue::Point { x, y }) => {
                if [*x, *y].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) { Ok(()) } else { Err(format!("{k} must have x and y in 0..1")) }
            }
            (ParamKind::Moment, ParamValue::Moment(m)) => {
                if m.0.flicks().unsigned_abs() <= MAX_KEY_FLICKS { Ok(()) } else { Err(format!("{k} is out of range")) }
            }
            (kind, _) => Err(format!("{k} must be a {}", kind.type_name())),
        }
    }
}

/// Checks `params` against `defs`: every key declared, every value valid.
pub fn validate(defs: &[ParamDef], params: &BTreeMap<String, ParamValue>) -> Result<(), String> {
    for (k, v) in params {
        let def = defs.iter().find(|d| d.key == k).ok_or_else(|| {
            format!("unknown parameter {k:?} (known: {})", defs.iter().map(|d| d.key).collect::<Vec<_>>().join(", "))
        })?;
        def.check(v)?;
    }
    Ok(())
}

/// The default of a parameter; `moment` is what a `Moment` defaults to.
pub fn default_value(def: &ParamDef, moment: SourceTime) -> ParamValue {
    match def.kind {
        ParamKind::Number { default, .. } => ParamValue::Number(default),
        ParamKind::Choice { default, .. } => ParamValue::Choice(default.to_string()),
        ParamKind::Point => ParamValue::Point { x: 0.5, y: 0.5 },
        ParamKind::Moment => ParamValue::Moment(moment),
    }
}

/// The values one `compile` sees: each declared parameter at this frame —
/// a number's `fx:` track when there is one, else the stored value; a
/// missing or invalid value falls back to the default, so a file from
/// elsewhere never breaks a frame. Numbers are clamped into range, points
/// into 0..1; a missing moment stays `None` (the recipe treats it as never).
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    values: Vec<(&'static str, Option<ParamValue>)>,
}

impl Params {
    pub fn resolve(defs: &[ParamDef], stored: &BTreeMap<String, ParamValue>, animated: impl Fn(&str) -> Option<f64>) -> Params {
        let values = defs
            .iter()
            .map(|d| {
                let v = match d.kind {
                    ParamKind::Number { min, max, default } => {
                        let n = animated(d.key).or_else(|| stored.get(d.key).and_then(ParamValue::as_number)).filter(|n| n.is_finite()).unwrap_or(default);
                        Some(ParamValue::Number(n.clamp(min, max)))
                    }
                    ParamKind::Choice { options, default } => Some(ParamValue::Choice(match stored.get(d.key) {
                        Some(ParamValue::Choice(c)) if options.contains(&c.as_str()) => c.clone(),
                        _ => default.to_string(),
                    })),
                    ParamKind::Point => Some(match stored.get(d.key) {
                        Some(ParamValue::Point { x, y }) if x.is_finite() && y.is_finite() => ParamValue::Point { x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) },
                        _ => ParamValue::Point { x: 0.5, y: 0.5 },
                    }),
                    ParamKind::Moment => match stored.get(d.key) {
                        Some(ParamValue::Moment(m)) => Some(ParamValue::Moment(*m)),
                        _ => None,
                    },
                };
                (d.key, v)
            })
            .collect();
        Params { values }
    }

    fn get(&self, key: &str) -> Option<&ParamValue> {
        self.values.iter().find(|(k, _)| *k == key).and_then(|(_, v)| v.as_ref())
    }

    /// A number parameter (0 when the recipe has none of that name).
    pub fn number(&self, key: &str) -> f64 {
        self.get(key).and_then(ParamValue::as_number).unwrap_or(0.0)
    }

    pub fn choice(&self, key: &str) -> &str {
        match self.get(key) {
            Some(ParamValue::Choice(c)) => c,
            _ => "",
        }
    }

    pub fn point(&self, key: &str) -> (f64, f64) {
        match self.get(key) {
            Some(ParamValue::Point { x, y }) => (*x, *y),
            _ => (0.5, 0.5),
        }
    }

    pub fn moment(&self, key: &str) -> Option<SourceTime> {
        match self.get(key) {
            Some(ParamValue::Moment(m)) => Some(*m),
            _ => None,
        }
    }

    /// Strength 0: the effect contributes nothing — not even a compile
    /// (spec §3). A recipe without a strength is never off.
    pub fn is_off(&self) -> bool {
        self.get("strength").and_then(ParamValue::as_number).is_some_and(|s| s <= 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::Time;

    const DEFS: [ParamDef; 4] = [
        ParamDef { key: "strength", kind: ParamKind::Number { min: 0.0, max: 1.0, default: 0.6 } },
        ParamDef { key: "direction", kind: ParamKind::Choice { options: &["in", "out"], default: "in" } },
        ParamDef { key: "point", kind: ParamKind::Point },
        ParamDef { key: "moment", kind: ParamKind::Moment },
    ];

    fn map(v: &[(&str, ParamValue)]) -> BTreeMap<String, ParamValue> {
        v.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn validation_names_the_key_and_the_reason() {
        assert_eq!(validate(&DEFS, &map(&[("strength", ParamValue::Number(0.3)), ("point", ParamValue::Point { x: 0.0, y: 1.0 })])), Ok(()));
        let err = |v: &[(&str, ParamValue)]| validate(&DEFS, &map(v)).unwrap_err();
        assert!(err(&[("strength", ParamValue::Number(1.2))]).contains("strength must be 0..1"));
        assert!(err(&[("strength", ParamValue::Number(f64::NAN))]).contains("strength"));
        assert!(err(&[("direction", ParamValue::Choice("left".into()))]).contains("one of in, out"));
        assert!(err(&[("point", ParamValue::Point { x: 1.5, y: 0.5 })]).contains("point"));
        assert!(err(&[("moment", ParamValue::Number(1.0))]).contains("moment must be a moment"));
        assert!(err(&[("speed", ParamValue::Number(1.0))]).contains("unknown parameter \"speed\""));
        assert!(err(&[("moment", ParamValue::Moment(SourceTime(Time(i64::MAX))))]).contains("out of range"));
    }

    #[test]
    fn resolve_fills_defaults_sanitises_and_prefers_tracks() {
        let stored = map(&[
            ("strength", ParamValue::Number(0.3)),
            ("direction", ParamValue::Choice("sideways".into())),
            ("point", ParamValue::Point { x: 1.7, y: -2.0 }),
        ]);
        let p = Params::resolve(&DEFS, &stored, |_| None);
        assert_eq!((p.number("strength"), p.choice("direction"), p.point("point"), p.moment("moment")), (0.3, "in", (1.0, 0.0), None));
        let tracked = Params::resolve(&DEFS, &stored, |k| (k == "strength").then_some(7.0));
        assert_eq!(tracked.number("strength"), 1.0, "a track wins and is clamped into range");
        let bare = Params::resolve(&DEFS, &BTreeMap::new(), |_| None);
        assert_eq!((bare.number("strength"), bare.point("point")), (0.6, (0.5, 0.5)), "defaults");
        assert_eq!(bare.number("nonexistent"), 0.0);
    }

    #[test]
    fn strength_zero_turns_an_effect_off_and_intensity_is_relative_to_normal() {
        assert!(Params::resolve(&DEFS, &map(&[("strength", ParamValue::Number(0.0))]), |_| None).is_off());
        assert!(!Params::resolve(&DEFS, &BTreeMap::new(), |_| None).is_off());
        assert!(!Params::resolve(&DEFS[1..], &BTreeMap::new(), |_| None).is_off(), "no strength parameter: never off");
        assert_eq!(intensity(NORMAL), 1.0);
        assert!((intensity(1.0) - 1.0 / 0.6).abs() < 1e-12);
        assert_eq!(default_value(&DEFS[3], SourceTime(Time::from_secs(2))), ParamValue::Moment(SourceTime(Time::from_secs(2))));
        assert_eq!(DEFS[0].label_key(), "fx.param.strength");
        assert!(DEFS[0].animatable() && !DEFS[1].animatable());
    }
}
