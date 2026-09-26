//! Every translation key referenced from the UI (.slint) or Rust code must
//! exist in every catalog. Catches untranslated strings at test time.

use kadr_i18n::{all_keys, Lang};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

/// Extracts string literals following any of `prefixes` (e.g. `t("`).
fn keys_after(src: &str, prefixes: &[&str]) -> Vec<String> {
    let mut out = vec![];
    for p in prefixes {
        let mut rest = src;
        while let Some(i) = rest.find(p) {
            // Must not be the tail of a longer identifier (e.g. `format!("`).
            let before = rest[..i].chars().last();
            let tail = &rest[i + p.len()..];
            if before.is_none_or(|c| !c.is_alphanumeric() && c != '_') {
                if let Some(end) = tail.find('"') {
                    let k = &tail[..end];
                    if !k.is_empty() && !k.ends_with('.') && k.contains('.') && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_') {
                        out.push(k.to_string());
                    }
                }
            }
            rest = tail;
        }
    }
    out
}

#[test]
fn all_referenced_keys_are_translated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut plain = BTreeSet::new();
    let mut plural = BTreeSet::new();

    let mut slint = vec![];
    files(&root.join("apps/editor/ui"), "slint", &mut slint);
    assert!(!slint.is_empty());
    for f in slint {
        let s = std::fs::read_to_string(f).unwrap();
        plain.extend(keys_after(&s, &["Tr.t(\"", "Tr.t1(\"", "Tr.t2(\""]));
    }
    let mut rs = vec![];
    for d in ["apps/editor/src", "crates/ai/src", "crates/timeline/src"] {
        files(&root.join(d), "rs", &mut rs);
    }
    for f in rs {
        let s = std::fs::read_to_string(f).unwrap();
        plain.extend(keys_after(&s, &["t(\"", "tf(\"", "t(&\"", "t_pos(\""]));
        plural.extend(keys_after(&s, &["tn(\""]));
        // Keys passed around as data (command labels, error keys, …).
        for pre in ["\"cmd.", "\"err.edit.", "\"privacy.deny.", "\"budget."] {
            for k in keys_after(&s, &[&pre[..1]]) {
                if k.starts_with(&pre[1..]) {
                    plain.insert(k);
                }
            }
        }
    }
    assert!(plain.len() > 300, "scanner found too few keys ({}) — is it broken?", plain.len());

    for lang in Lang::ALL {
        let have: BTreeSet<&str> = all_keys(lang).into_iter().collect();
        let forms: &[&str] = if lang == Lang::Ru { &["one", "few", "many"] } else { &["one", "other"] };
        let mut missing: Vec<String> = plain.iter().filter(|k| !have.contains(k.as_str())).cloned().collect();
        for k in &plural {
            for f in forms {
                let full = format!("{k}.{f}");
                if !have.contains(full.as_str()) {
                    missing.push(full);
                }
            }
        }
        assert!(missing.is_empty(), "{lang:?} catalog is missing {} key(s): {missing:#?}", missing.len());
    }
}
