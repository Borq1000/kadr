//! Embeds every .ttf / .otf under `fonts/` (titles spec §5.1) as
//! `BUNDLED_FILES: &[(&str, &[u8])]` in `$OUT_DIR/bundled.rs`.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("fonts");
    println!("cargo:rerun-if-changed=fonts");
    let mut files = vec![];
    for dir in std::fs::read_dir(&root).expect("crates/text/fonts (Task 3, Step 1)") {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            let ext = f.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("ttf" | "otf")) {
                files.push(f);
            }
        }
    }
    files.sort();
    let mut out = String::from("/// Every bundled font file: (path under crates/text/fonts, bytes).\npub static BUNDLED_FILES: &[(&str, &[u8])] = &[\n");
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        writeln!(out, "    ({rel:?}, include_bytes!({:?})),", f.to_string_lossy()).unwrap();
    }
    out.push_str("];\n");
    std::fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("bundled.rs"), out).unwrap();
}
