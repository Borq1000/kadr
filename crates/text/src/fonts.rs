//! Fonts (titles spec §2.4, §5.1): one collection per process — the
//! bundled families (embedded), Windows' fonts (DirectWrite: the system's
//! and the user's own installs) and the fonts added to Kadr's data folder.
//! A family the collection lacks is drawn in [`DEFAULT_FAMILY`]; characters
//! a font lacks come from a system font for their script (parley and
//! fontique choose it), emoji from Segoe UI Emoji.

use crate::layout::RunIx;
use parking_lot::Mutex;
use parley::fontique::{Blob, Collection, CollectionOptions, SourceCache};
use parley::{FontContext, LayoutContext};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

include!(concat!(env!("OUT_DIR"), "/bundled.rs"));

/// The family of a new text and the stand-in for a missing one.
pub const DEFAULT_FAMILY: &str = "Montserrat";

/// The bundled families in the picker's order (their names as the fonts
/// declare them; Task 3's tests check each resolves).
pub const BUNDLED_FAMILIES: &[&str] = &[
    "Montserrat", "Inter", "Rubik", "Oswald", "Roboto Condensed", "Playfair Display", "PT Serif",
    "Lobster", "Comfortaa", "Unbounded", "Caveat", "Russo One", "Press Start 2P", "Exo 2",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontSource {
    Bundled,
    User,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub source: FontSource,
}

pub(crate) struct Cx {
    pub fonts: FontContext,
    pub layout: LayoutContext<RunIx>,
}

pub struct Fonts {
    cx: Mutex<Cx>,
    user: Mutex<Vec<String>>,
    user_dir: Mutex<Option<PathBuf>>,
    generation: AtomicU64,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// The process's fonts; the first call builds the collection (about 1 ms
/// warm, 20 ms cold — the editor warms it on a background thread).
pub fn fonts() -> &'static Fonts {
    FONTS.get_or_init(Fonts::load)
}

impl Fonts {
    fn load() -> Fonts {
        let mut collection = Collection::new(CollectionOptions { shared: false, system_fonts: true });
        for (_, bytes) in BUNDLED_FILES {
            collection.register_fonts(Blob::new(Arc::new(*bytes)), None);
        }
        Fonts {
            cx: Mutex::new(Cx { fonts: FontContext { collection, source_cache: SourceCache::default() }, layout: LayoutContext::new() }),
            user: Mutex::new(vec![]),
            user_dir: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }

    pub(crate) fn with_cx<R>(&self, f: impl FnOnce(&mut Cx) -> R) -> R {
        f(&mut self.cx.lock())
    }

    /// Bumped whenever a font is added: cached layouts made before are stale.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn has(&self, family: &str) -> bool {
        self.cx.lock().fonts.collection.family_id(family).is_some()
    }

    /// Every family to pick from: the bundled ones (in [`BUNDLED_FAMILIES`]
    /// order), the user's, then the system's (sorted), each once.
    pub fn families(&self) -> Vec<Family> {
        let names: BTreeSet<String> = {
            let mut cx = self.cx.lock();
            let c = &mut cx.fonts.collection;
            let all: Vec<String> = c.family_names().map(str::to_owned).collect();
            let mut canon = BTreeSet::new();
            for n in &all {
                if let Some(id) = c.family_id(n)
                    && let Some(name) = c.family_name(id)
                {
                    canon.insert(name.to_owned());
                }
            }
            canon
        };
        let user = self.user.lock().clone();
        let taken = |n: &str| BUNDLED_FAMILIES.iter().any(|b| b.eq_ignore_ascii_case(n)) || user.iter().any(|u| u.eq_ignore_ascii_case(n));
        let mut out: Vec<Family> = BUNDLED_FAMILIES.iter().map(|n| Family { name: n.to_string(), source: FontSource::Bundled }).collect();
        out.extend(user.iter().map(|n| Family { name: n.clone(), source: FontSource::User }));
        out.extend(names.into_iter().filter(|n| !taken(n) && !n.starts_with('@')).map(|name| Family { name, source: FontSource::System }));
        out
    }

    /// Registers a font file's bytes; returns the families it holds (none:
    /// not a font). Layouts are refreshed (`generation`).
    pub fn add_font_bytes(&self, bytes: Vec<u8>) -> Vec<String> {
        let names: Vec<String> = {
            let mut cx = self.cx.lock();
            let c = &mut cx.fonts.collection;
            let ids: Vec<_> = c.register_fonts(Blob::from(bytes), None).into_iter().map(|(id, _)| id).collect();
            ids.into_iter().filter_map(|id| c.family_name(id).map(str::to_owned)).collect()
        };
        if !names.is_empty() {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        names
    }

    /// Registers every .ttf / .otf / .ttc in `dir` as the user's fonts and
    /// remembers `dir` for [`Fonts::install_user_font`]; returns how many
    /// files were fonts.
    pub fn load_user_dir(&self, dir: &Path) -> usize {
        *self.user_dir.lock() = Some(dir.to_path_buf());
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        let mut n = 0;
        for f in entries.flatten().map(|e| e.path()) {
            let ext = f.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&f) else { continue };
            let names = self.add_font_bytes(bytes);
            if !names.is_empty() {
                n += 1;
                self.remember_user(names);
            } else {
                tracing::warn!(file = %f.display(), "not a font: skipped");
            }
        }
        n
    }

    /// «Добавить шрифт…»: registers the file and copies it into the user
    /// font folder (when one was loaded), so it is there after a restart.
    pub fn install_user_font(&self, file: &Path) -> Result<Vec<String>, String> {
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        let names = self.add_font_bytes(bytes);
        if names.is_empty() {
            return Err("not a font".into());
        }
        if let Some(dir) = self.user_dir.lock().clone() {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let name = file.file_name().map(|n| n.to_owned()).unwrap_or_else(|| "font.ttf".into());
            let target = dir.join(name);
            if !target.exists() {
                std::fs::copy(file, &target).map_err(|e| e.to_string())?;
            }
        }
        self.remember_user(names.clone());
        Ok(names)
    }

    fn remember_user(&self, names: Vec<String>) {
        let mut user = self.user.lock();
        for n in names {
            if !user.iter().any(|u| u.eq_ignore_ascii_case(&n)) && !BUNDLED_FAMILIES.iter().any(|b| b.eq_ignore_ascii_case(&n)) {
                user.push(n);
            }
        }
        user.sort_by_key(|n| n.to_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_cover_cyrillic() {
        assert!(BUNDLED_FILES.len() >= 14, "Step 1 fetched the fonts: {}", BUNDLED_FILES.len());
        for (name, bytes) in BUNDLED_FILES {
            let font = swash::FontRef::from_index(bytes, 0).unwrap_or_else(|| panic!("{name} is a font"));
            let cmap = font.charmap();
            for ch in ['Ж', 'я', 'Ё', 'A', 'z', '0', '!'] {
                assert_ne!(cmap.map(ch), 0, "{name} lacks {ch:?}");
            }
        }
    }

    #[test]
    fn bundled_fonts_fit_the_budget_and_carry_their_licences() {
        let total: usize = BUNDLED_FILES.iter().map(|(_, b)| b.len()).sum();
        assert!(total <= 12 << 20, "{} MB of fonts", total >> 20);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
        for dir in std::fs::read_dir(&root).unwrap().map(|d| d.unwrap().path()).filter(|d| d.is_dir()) {
            assert!(dir.join("OFL.txt").exists() || dir.join("LICENSE.txt").exists(), "{} has no licence file", dir.display());
        }
    }

    #[test]
    fn every_bundled_family_is_known_by_name_and_listed_first() {
        let f = fonts();
        for name in BUNDLED_FAMILIES {
            assert!(f.has(name), "{name}");
        }
        let all = f.families();
        let firsts: Vec<&str> = all.iter().take(BUNDLED_FAMILIES.len()).map(|x| x.name.as_str()).collect();
        assert_eq!(firsts, BUNDLED_FAMILIES);
        assert!(all.iter().take(BUNDLED_FAMILIES.len()).all(|x| x.source == FontSource::Bundled));
        assert!(all.iter().any(|x| x.source == FontSource::System && x.name == "Segoe UI"), "Windows' own fonts are listed");
        let mut names: Vec<String> = all.iter().map(|x| x.name.to_lowercase()).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "each family once");
        assert!(!f.has("No Such Font Anywhere"));
    }

    #[test]
    fn a_font_file_can_be_added_and_garbage_cannot() {
        let _serial = crate::TEST_FONTS.lock();
        let f = fonts();
        let g0 = f.generation();
        let (_, lobster) = BUNDLED_FILES.iter().find(|(n, _)| n.starts_with("lobster/")).expect("Lobster");
        assert!(f.add_font_bytes(lobster.to_vec()).iter().any(|n| n == "Lobster"), "registering a font returns its families");
        assert!(f.generation() > g0, "layouts made before are stale");
        assert!(f.add_font_bytes(b"not a font".to_vec()).is_empty());
        let dir = std::env::temp_dir().join(format!("kadr-text-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let junk = dir.join("junk.ttf");
        std::fs::write(&junk, b"nope").unwrap();
        assert!(f.install_user_font(&junk).is_err(), "a file that is not a font is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
