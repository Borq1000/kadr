//! Diagnoses multicam audio sync on two cached audio overviews.
//! Usage: cargo run -p kadr-analysis --example sync_probe -- <a/overview.bin> <b/overview.bin>

use kadr_analysis::sync::align_envelopes;
use kadr_analysis::AudioOverview;
use kadr_core::Time;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let load = |p: &str| AudioOverview::from_bytes(&std::fs::read(p).expect("read")).expect("parse");
    let (a, b) = (load(&args[0]), load(&args[1]));
    let stats = |v: &[f32]| {
        let min = v.iter().copied().fold(f32::MAX, f32::min);
        let max = v.iter().copied().fold(f32::MIN, f32::max);
        (v.len(), min, max)
    };
    println!("a: {:?}  b: {:?}", stats(&a.levels_db), stats(&b.levels_db));
    println!("a[300..320] {:?}", &a.levels_db[300..320]);
    let r = align_envelopes(&a.levels_db, &b.levels_db, Time::from_millis(10), Time::from_secs(600));
    println!("result: {r:?}");
}
