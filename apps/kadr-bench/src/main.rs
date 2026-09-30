//! Kadr performance harness (render spec §10): measures instead of guessing.
//! Not shipped.
//!
//!   kadr-bench baseline [--quick]                  legacy decode, seek, copy and export numbers
//!   kadr-bench live --clip <file> [--seconds N]    the real app, headless, through kadr-mcp
//!   kadr-bench scene                               cost of the scene evaluator (M1)
//!   kadr-bench render                              CpuRenderer at 1080p and 4K, 1-3 layers and a transition (M2)
//!   kadr-bench avsync-selftest                    A/V sync harness on its own 60 s source (spec §8)
//!   kadr-bench avsync-analyze <file> [--fps N/D]   flash/click offsets of any file (default 30000/1001)

mod alloc;
mod avsync;
mod baseline;
mod live;
mod media;
mod render_bench;
mod report;
mod scene_bench;

#[global_allocator]
static ALLOC: alloc::Counting = alloc::Counting;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let result = match args.first().map(String::as_str) {
        Some("baseline") => baseline::run(flag("--quick")),
        Some("live") => {
            let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
            match value("--clip") {
                Some(c) => live::run(std::path::Path::new(&c), value("--seconds").and_then(|s| s.parse().ok()).unwrap_or(20)),
                None => Err("live needs --clip <file>".to_string()),
            }
        }
        Some("scene") => scene_bench::run(),
        Some("render") => render_bench::run(),
        Some("avsync-selftest") => avsync::selftest(),
        Some("avsync-analyze") => {
            let fps = args.iter().position(|a| a == "--fps").and_then(|i| args.get(i + 1)).map_or(Some(kadr_core::FrameRate::FPS_29_97), |s| kadr_core::FrameRate::parse(s));
            match (args.get(1).filter(|a| !a.starts_with("--")), fps) {
                (Some(f), Some(fps)) => avsync::analyze_file(std::path::Path::new(f), fps),
                (None, _) => Err("avsync-analyze needs <file>".to_string()),
                (_, None) => Err("avsync-analyze: bad --fps".to_string()),
            }
        }
        _ => Err("usage: kadr-bench baseline [--quick] | live --clip <file> [--seconds N] | scene | render | avsync-selftest | avsync-analyze <file> [--fps N/D]".to_string()),
    };
    match result {
        Ok(r) => {
            println!("{}", report::markdown(&r));
            match report::save(&r) {
                Ok(p) => println!("saved {}", p.display()),
                Err(e) => eprintln!("kadr-bench: could not save report: {e}"),
            }
        }
        Err(e) => {
            eprintln!("kadr-bench: {e}");
            std::process::exit(1);
        }
    }
}
