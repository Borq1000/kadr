//! Kadr performance harness (render spec §10): measures instead of guessing.
//! Not shipped.
//!
//!   kadr-bench baseline [--quick]                  seek, decode and buffer-copy numbers (scaling stream)
//!   kadr-bench live --clip <file> [--seconds N] [--layers 1|3] [--quality full|half|quarter]
//!                                                  the real app, headless, through kadr-mcp (3 layers: clip + rotated 4K PiP + logo)
//!   kadr-bench scene                               cost of the scene evaluator (M1)
//!   kadr-bench render                              CpuRenderer at 1080p and 4K, 1-3 layers and a transition (M2)
//!   kadr-bench playback [--quick]                 seek latency and sequential decode through Resolver + CpuRenderer (M3)
//!   kadr-bench export [--diag]                    export on the render pipeline: fps rows and fast-path diagnosis (M5)
//!   kadr-bench avsync-export                      20-minute cut-up timeline exported and analyzed for A/V sync (M5)
//!   kadr-bench avsync-selftest                    A/V sync harness on its own 60 s source (spec §8)
//!   kadr-bench avsync-analyze <file> [--fps N/D]   flash/click offsets of any file (default 30000/1001)

mod alloc;
mod avsync;
mod baseline;
mod export_bench;
mod live;
mod media;
mod playback_bench;
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
                Some(c) => match (value("--layers").map_or(Some(1), |s| s.parse::<u32>().ok().filter(|n| [1, 3].contains(n))), value("--quality").map_or(Some(1), |s| live::parse_quality(&s))) {
                    (Some(layers), Some(quality)) => live::run(std::path::Path::new(&c), value("--seconds").and_then(|s| s.parse().ok()).unwrap_or(20), &live::Options { layers, quality }),
                    _ => Err("live: --layers is 1 or 3, --quality is full|half|quarter".to_string()),
                },
                None => Err("live needs --clip <file>".to_string()),
            }
        }
        Some("scene") => scene_bench::run(),
        Some("render") => render_bench::run(),
        Some("playback") => playback_bench::run(flag("--quick")),
        Some("export") => export_bench::run(flag("--diag")),
        Some("avsync-export") => export_bench::run_avsync(),
        Some("avsync-selftest") => avsync::selftest(),
        Some("avsync-analyze") => {
            let fps = args.iter().position(|a| a == "--fps").and_then(|i| args.get(i + 1)).map_or(Some(kadr_core::FrameRate::FPS_29_97), |s| kadr_core::FrameRate::parse(s));
            match (args.get(1).filter(|a| !a.starts_with("--")), fps) {
                (Some(f), Some(fps)) => avsync::analyze_file(std::path::Path::new(f), fps),
                (None, _) => Err("avsync-analyze needs <file>".to_string()),
                (_, None) => Err("avsync-analyze: bad --fps".to_string()),
            }
        }
        _ => Err("usage: kadr-bench baseline [--quick] | live --clip <file> [--seconds N] [--layers 1|3] [--quality full|half|quarter] | scene | render | playback [--quick] | export [--diag] | avsync-export | avsync-selftest | avsync-analyze <file> [--fps N/D]".to_string()),
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
