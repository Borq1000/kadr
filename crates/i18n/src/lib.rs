//! Localization. One flat JSON catalog per language (`locales/*.json`),
//! embedded in the binary, shared by Rust and the Slint UI.
//!
//! - `t("key")` — plain string
//! - `tf("key", &[("name", v)])` — `{name}` placeholders
//! - `tn("key", n, args)` — plural forms: `key.one` / `key.few` / `key.many`
//!   (Russian) or `key.one` / `key.other` (English); `{n}` is filled in.
//!
//! Missing keys fall back to English, then to the key itself (visible in
//! the UI so gaps are noticed); tests guarantee both catalogs are complete.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Ru, Lang::En];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }
    pub fn from_code(c: &str) -> Option<Lang> {
        match c.get(..2)?.to_ascii_lowercase().as_str() {
            "en" => Some(Lang::En),
            "ru" | "uk" | "be" | "kk" => Some(Lang::Ru),
            _ => None,
        }
    }
    /// Native name for the language picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ru => "Русский",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

type Catalog = HashMap<String, String>;

fn catalogs() -> &'static (Catalog, Catalog) {
    static C: OnceLock<(Catalog, Catalog)> = OnceLock::new();
    C.get_or_init(|| {
        let en: Catalog = serde_json::from_str(include_str!("../locales/en.json")).expect("en.json is valid");
        let ru: Catalog = serde_json::from_str(include_str!("../locales/ru.json")).expect("ru.json is valid");
        (en, ru)
    })
}

pub fn set_lang(l: Lang) {
    CURRENT.store(l as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == Lang::Ru as u8 { Lang::Ru } else { Lang::En }
}

fn raw(key: &str) -> Option<&'static str> {
    let (en, ru) = catalogs();
    let primary = if lang() == Lang::Ru { ru } else { en };
    primary.get(key).or_else(|| en.get(key)).map(String::as_str)
}

pub fn t(key: &str) -> String {
    raw(key).map(str::to_string).unwrap_or_else(|| key.to_string())
}

pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    fill(t(key), args)
}

/// Plural-aware lookup.
pub fn tn(key: &str, n: i64, args: &[(&str, &str)]) -> String {
    let form = plural_form(lang(), n);
    let k = format!("{key}.{form}");
    let s = raw(&k).or_else(|| raw(&format!("{key}.other"))).or_else(|| raw(&format!("{key}.many")));
    let s = s.map(str::to_string).unwrap_or(k);
    let n_str = n.to_string();
    let mut all: Vec<(&str, &str)> = vec![("n", n_str.as_str())];
    all.extend_from_slice(args);
    fill(s, &all)
}

pub fn plural_form(l: Lang, n: i64) -> &'static str {
    let n = n.unsigned_abs();
    match l {
        Lang::En => {
            if n == 1 { "one" } else { "other" }
        }
        Lang::Ru => {
            let (m10, m100) = (n % 10, n % 100);
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "many"
            }
        }
    }
}

fn fill(mut s: String, args: &[(&str, &str)]) -> String {
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Positional form used by the Slint bridge: `{0}`, `{1}`.
pub fn t_pos(key: &str, args: &[&str]) -> String {
    let mut s = t(key);
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

/// Human duration in the current language: `1 ч 47 мин`, `3m 05s`, `12.4 s`.
pub fn duration(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs >= 3600.0 {
        let m = (secs / 60.0).floor() as u64;
        tf("dur.hm", &[("h", &(m / 60).to_string()), ("m", &format!("{:02}", m % 60))])
    } else if secs >= 60.0 {
        let s = secs.floor() as u64;
        tf("dur.ms", &[("m", &(s / 60).to_string()), ("s", &format!("{:02}", s % 60))])
    } else {
        let v = format!("{secs:.1}");
        let v = if lang() == Lang::Ru { v.replace('.', ",") } else { v };
        tf("dur.s", &[("s", &v)])
    }
}

pub fn all_keys(l: Lang) -> Vec<&'static str> {
    let (en, ru) = catalogs();
    let c = if l == Lang::Ru { ru } else { en };
    c.keys().map(String::as_str).collect()
}

/// System UI language (Windows), falling back to `LANG`.
pub fn system_lang() -> Lang {
    #[cfg(windows)]
    {
        let mut buf = [0u16; 85];
        // SAFETY: buffer is LOCALE_NAME_MAX_LENGTH (85) wide chars.
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if n > 1 {
            let name = String::from_utf16_lossy(&buf[..(n - 1) as usize]);
            if let Some(l) = Lang::from_code(&name) {
                return l;
            }
        }
    }
    std::env::var("LANG").ok().and_then(|v| Lang::from_code(&v)).unwrap_or(Lang::En)
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(l: Lang) -> BTreeSet<&'static str> {
        all_keys(l).into_iter().collect()
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(a) = rest.find('{') {
            if let Some(b) = rest[a..].find('}') {
                out.insert(rest[a + 1..a + b].to_string());
                rest = &rest[a + b + 1..];
            } else {
                break;
            }
        }
        out
    }

    #[test]
    fn catalogs_have_identical_keys() {
        // Plural keys differ in shape (en: one/other, ru: one/few/many);
        // compare plural stems separately.
        let norm = |k: &str| {
            for f in [".one", ".few", ".many", ".other"] {
                if let Some(s) = k.strip_suffix(f) {
                    return s.to_string();
                }
            }
            k.to_string()
        };
        let en: BTreeSet<String> = keys(Lang::En).into_iter().map(norm).collect();
        let ru: BTreeSet<String> = keys(Lang::Ru).into_iter().map(norm).collect();
        let missing_ru: Vec<_> = en.difference(&ru).collect();
        let missing_en: Vec<_> = ru.difference(&en).collect();
        assert!(missing_ru.is_empty(), "missing in ru.json: {missing_ru:?}");
        assert!(missing_en.is_empty(), "missing in en.json: {missing_en:?}");
    }

    #[test]
    fn plural_catalog_shapes() {
        for k in keys(Lang::Ru) {
            if let Some(stem) = k.strip_suffix(".one") {
                for f in ["few", "many"] {
                    assert!(keys(Lang::Ru).contains(format!("{stem}.{f}").as_str()), "ru plural {stem}.{f} missing");
                }
                assert!(keys(Lang::En).contains(format!("{stem}.other").as_str()), "en plural {stem}.other missing");
            }
        }
    }

    #[test]
    fn placeholders_match_between_languages() {
        let (en, ru) = catalogs();
        for (k, v) in en {
            if let Some(r) = ru.get(k) {
                assert_eq!(placeholders(v), placeholders(r), "placeholder mismatch for {k}");
            }
        }
    }

    #[test]
    fn russian_plurals() {
        let f = |n| plural_form(Lang::Ru, n);
        assert_eq!([f(1), f(2), f(5), f(11), f(21), f(22), f(25), f(112)], ["one", "few", "many", "many", "one", "few", "many", "many"]);
        assert_eq!(plural_form(Lang::En, 1), "one");
        assert_eq!(plural_form(Lang::En, 0), "other");
    }

    #[test]
    fn lookup_and_fallback() {
        set_lang(Lang::Ru);
        assert_ne!(t("app.name"), "app.name");
        assert_eq!(t("no.such.key"), "no.such.key");
        set_lang(Lang::En);
    }
}
