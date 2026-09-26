//! Integer timeline time.
//!
//! All timeline arithmetic is done in **flicks** (1/705_600_000 s). A flick
//! divides evenly into one frame at every common video rate (23.976, 24, 25,
//! 29.97, 30, 48, 50, 59.94, 60, 120 fps) and into one sample at every common
//! audio rate (8–192 kHz), so cutting on frame or sample boundaries never
//! accumulates rounding error. `i64` flicks cover ±414 years.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

pub const FLICKS_PER_SECOND: i64 = 705_600_000;
const FLICKS_PER_MS: i64 = FLICKS_PER_SECOND / 1000;

/// A point or duration on a timeline, in flicks.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Time(pub i64);

impl Time {
    pub const ZERO: Time = Time(0);
    pub const MAX: Time = Time(i64::MAX);

    pub const fn from_flicks(f: i64) -> Self {
        Time(f)
    }
    pub const fn flicks(self) -> i64 {
        self.0
    }
    pub const fn from_secs(s: i64) -> Self {
        Time(s * FLICKS_PER_SECOND)
    }
    pub const fn from_millis(ms: i64) -> Self {
        Time(ms * FLICKS_PER_MS)
    }
    /// A flick is not a whole number of microseconds (1 µs = 705.6 flicks),
    /// so this rounds to the nearest flick.
    pub fn from_micros(us: i64) -> Self {
        Time(mul_div_round(us, FLICKS_PER_SECOND, 1_000_000))
    }
    /// Converts from floating seconds, rounding to the nearest flick. Only
    /// for boundaries (UI pixels, probe output) — never for edit arithmetic.
    pub fn from_secs_f64(s: f64) -> Self {
        Time((s * FLICKS_PER_SECOND as f64).round() as i64)
    }
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / FLICKS_PER_SECOND as f64
    }
    /// Milliseconds, rounded toward negative infinity.
    pub const fn as_millis(self) -> i64 {
        self.0.div_euclid(FLICKS_PER_MS)
    }
    /// Microseconds, rounded to nearest (1 µs is not a whole number of
    /// flicks, so floor would turn `from_micros(7)` into 6 µs).
    pub fn as_micros(self) -> i64 {
        mul_div_round(self.0, 1_000_000, FLICKS_PER_SECOND)
    }
    pub fn abs(self) -> Self {
        Time(self.0.abs())
    }
    pub fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Number of whole samples at `rate` Hz that fit before this time.
    pub fn to_samples(self, rate: u32) -> i64 {
        mul_div_floor(self.0, rate as i64, FLICKS_PER_SECOND)
    }
    pub fn from_samples(samples: i64, rate: u32) -> Self {
        Time(mul_div_floor(samples, FLICKS_PER_SECOND, rate as i64))
    }

    /// Scales a duration by a speed factor expressed as a ratio (e.g. 2/1 is
    /// double speed → half duration).
    pub fn div_ratio(self, num: i64, den: i64) -> Self {
        Time(mul_div_floor(self.0, den, num))
    }
    pub fn mul_ratio(self, num: i64, den: i64) -> Self {
        Time(mul_div_floor(self.0, num, den))
    }

    /// Formats as an FFmpeg time argument (`seconds.micros`), exact to 1 µs.
    pub fn to_ffmpeg_arg(self) -> String {
        let us = self.as_micros();
        let sign = if us < 0 { "-" } else { "" };
        let us = us.unsigned_abs();
        format!("{sign}{}.{:06}", us / 1_000_000, us % 1_000_000)
    }
}

/// `a * b / c` computed in i128 with floor rounding.
fn mul_div_floor(a: i64, b: i64, c: i64) -> i64 {
    let n = a as i128 * b as i128;
    n.div_euclid(c as i128) as i64
}

fn mul_div_round(a: i64, b: i64, c: i64) -> i64 {
    let n = a as i128 * b as i128;
    let c = c as i128;
    (n + c / 2).div_euclid(c) as i64
}

impl Add for Time {
    type Output = Time;
    fn add(self, o: Time) -> Time {
        Time(self.0 + o.0)
    }
}
impl Sub for Time {
    type Output = Time;
    fn sub(self, o: Time) -> Time {
        Time(self.0 - o.0)
    }
}
impl AddAssign for Time {
    fn add_assign(&mut self, o: Time) {
        self.0 += o.0;
    }
}
impl SubAssign for Time {
    fn sub_assign(&mut self, o: Time) {
        self.0 -= o.0;
    }
}
impl Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6}s", self.as_secs_f64())
    }
}

/// Half-open time interval `[start, end)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Time,
    pub end: Time,
}

impl TimeRange {
    pub fn new(start: Time, end: Time) -> Self {
        debug_assert!(start <= end, "inverted range {start:?}..{end:?}");
        TimeRange { start, end }
    }
    pub fn duration(&self) -> Time {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
    pub fn contains(&self, t: Time) -> bool {
        t >= self.start && t < self.end
    }
    pub fn overlaps(&self, o: &TimeRange) -> bool {
        self.start < o.end && o.start < self.end
    }
    pub fn intersect(&self, o: &TimeRange) -> Option<TimeRange> {
        let s = self.start.max(o.start);
        let e = self.end.min(o.end);
        (s < e).then(|| TimeRange::new(s, e))
    }
}

/// Rational frame rate, e.g. 30000/1001 for 29.97 fps.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const FPS_23_976: FrameRate = FrameRate::new(24000, 1001);
    pub const FPS_24: FrameRate = FrameRate::new(24, 1);
    pub const FPS_25: FrameRate = FrameRate::new(25, 1);
    pub const FPS_29_97: FrameRate = FrameRate::new(30000, 1001);
    pub const FPS_30: FrameRate = FrameRate::new(30, 1);
    pub const FPS_50: FrameRate = FrameRate::new(50, 1);
    pub const FPS_59_94: FrameRate = FrameRate::new(60000, 1001);
    pub const FPS_60: FrameRate = FrameRate::new(60, 1);

    pub const fn new(num: u32, den: u32) -> Self {
        FrameRate { num, den }
    }

    /// Parses FFmpeg notation: `"30000/1001"`, `"25/1"`, `"25"`, `"29.97"`.
    /// Returns `None` for `0/0` (unknown / VFR marker).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let fr = if let Some((n, d)) = s.split_once('/') {
            FrameRate::new(n.trim().parse().ok()?, d.trim().parse().ok()?)
        } else if let Ok(n) = s.parse::<u32>() {
            FrameRate::new(n, 1)
        } else {
            return Self::from_f64(s.parse().ok()?);
        };
        (fr.num > 0 && fr.den > 0).then(|| fr.reduced())
    }

    /// Maps a float rate to the closest standard NTSC/integer rational.
    pub fn from_f64(fps: f64) -> Option<Self> {
        if !(fps.is_finite() && fps > 0.0) {
            return None;
        }
        let rounded = fps.round();
        let ntsc = rounded * 1000.0 / 1001.0;
        if (fps - ntsc).abs() < 0.005 && (fps - rounded).abs() > 0.005 {
            Some(FrameRate::new(rounded as u32 * 1000, 1001))
        } else if (fps - rounded).abs() < 0.005 {
            Some(FrameRate::new(rounded as u32, 1))
        } else {
            Some(FrameRate::new((fps * 1000.0).round() as u32, 1000).reduced())
        }
    }

    fn reduced(self) -> Self {
        fn gcd(a: u32, b: u32) -> u32 {
            if b == 0 { a } else { gcd(b, a % b) }
        }
        let g = gcd(self.num, self.den).max(1);
        FrameRate::new(self.num / g, self.den / g)
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Integer rate used for timecode counting (30 for 29.97).
    pub fn nominal(self) -> u32 {
        (self.as_f64().round() as u32).max(1)
    }

    /// NTSC rates that conventionally use drop-frame timecode.
    pub fn is_drop_frame(self) -> bool {
        self.den == 1001 && self.num % 30000 == 0
    }

    /// Start time of frame `n`. Exact for all standard rates.
    pub fn frame_to_time(self, n: i64) -> Time {
        Time(mul_div_floor(n, FLICKS_PER_SECOND * self.den as i64, self.num as i64))
    }

    /// Index of the frame that contains time `t`.
    pub fn time_to_frame(self, t: Time) -> i64 {
        mul_div_floor(t.0, self.num as i64, FLICKS_PER_SECOND * self.den as i64)
    }

    /// Index of the frame boundary nearest to `t`.
    pub fn time_to_frame_round(self, t: Time) -> i64 {
        mul_div_round(t.0, self.num as i64, FLICKS_PER_SECOND * self.den as i64)
    }

    /// Snaps `t` to the nearest frame boundary.
    pub fn snap(self, t: Time) -> Time {
        self.frame_to_time(self.time_to_frame_round(t))
    }

    pub fn frame_duration(self) -> Time {
        self.frame_to_time(1)
    }

    pub fn to_ffmpeg_arg(self) -> String {
        format!("{}/{}", self.num, self.den)
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        FrameRate::FPS_30
    }
}

impl fmt::Debug for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            let v = self.as_f64();
            let s = format!("{v:.3}");
            write!(f, "{}", s.trim_end_matches('0').trim_end_matches('.'))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [FrameRate; 8] = [
        FrameRate::FPS_23_976,
        FrameRate::FPS_24,
        FrameRate::FPS_25,
        FrameRate::FPS_29_97,
        FrameRate::FPS_30,
        FrameRate::FPS_50,
        FrameRate::FPS_59_94,
        FrameRate::FPS_60,
    ];

    #[test]
    fn frame_duration_is_exact_for_all_standard_rates() {
        for fr in ALL {
            let d = fr.frame_duration();
            // n frames == n * duration, with no drift, even over 10 hours.
            let n = (fr.as_f64() * 36_000.0) as i64;
            assert_eq!(fr.frame_to_time(n), Time(d.0 * n), "{fr:?}");
            assert_eq!(
                (FLICKS_PER_SECOND * fr.den as i64) % fr.num as i64,
                0,
                "flicks not divisible for {fr:?}"
            );
        }
    }

    #[test]
    fn frame_time_roundtrip() {
        for fr in ALL {
            for n in [0, 1, 2, 999, 1001, 86_399, 5_000_000] {
                let t = fr.frame_to_time(n);
                assert_eq!(fr.time_to_frame(t), n, "{fr:?} frame {n}");
                // Any time inside the frame maps back to the same frame.
                let mid = t + Time(fr.frame_duration().0 / 2);
                assert_eq!(fr.time_to_frame(mid), n);
                assert_eq!(fr.time_to_frame(fr.frame_to_time(n + 1) - Time(1)), n);
            }
        }
    }

    #[test]
    fn snap_to_nearest_frame() {
        let fr = FrameRate::FPS_25; // 40ms frames
        assert_eq!(fr.snap(Time::from_millis(19)), Time::ZERO);
        assert_eq!(fr.snap(Time::from_millis(21)), Time::from_millis(40));
        assert_eq!(fr.snap(Time::from_millis(-21)), Time::from_millis(-40));
    }

    #[test]
    fn ntsc_hour_is_not_an_integer_number_of_frames() {
        let fr = FrameRate::FPS_29_97;
        // One hour of real time holds 107892.107... frames.
        assert_eq!(fr.time_to_frame(Time::from_secs(3600)), 107_892);
    }

    #[test]
    fn parse_rates() {
        assert_eq!(FrameRate::parse("30000/1001"), Some(FrameRate::FPS_29_97));
        assert_eq!(FrameRate::parse("50/2"), Some(FrameRate::FPS_25));
        assert_eq!(FrameRate::parse("24"), Some(FrameRate::FPS_24));
        assert_eq!(FrameRate::parse("0/0"), None);
        assert_eq!(FrameRate::parse("23.976"), Some(FrameRate::FPS_23_976));
        assert_eq!(FrameRate::parse("59.94"), Some(FrameRate::FPS_59_94));
        assert_eq!(FrameRate::parse("12.5"), Some(FrameRate::new(25, 2)));
        assert_eq!(FrameRate::FPS_29_97.to_string(), "29.97");
        assert_eq!(FrameRate::FPS_23_976.to_string(), "23.976");
    }

    #[test]
    fn samples_exact() {
        for rate in [44_100u32, 48_000, 96_000] {
            let t = Time::from_secs(5400); // 90 minutes
            assert_eq!(t.to_samples(rate), 5400 * rate as i64);
            assert_eq!(Time::from_samples(t.to_samples(rate), rate), t);
        }
    }

    #[test]
    fn negative_division_floors() {
        assert_eq!(Time(-1).as_millis(), -1);
        assert_eq!(FrameRate::FPS_25.time_to_frame(Time(-1)), -1);
    }

    #[test]
    fn ffmpeg_args() {
        assert_eq!(Time::from_millis(12_500).to_ffmpeg_arg(), "12.500000");
        assert_eq!(Time::from_micros(-1_500_000).to_ffmpeg_arg(), "-1.500000");
        assert_eq!(FrameRate::FPS_29_97.to_ffmpeg_arg(), "30000/1001");
        // Round-trip through microseconds stays within one flick.
        for us in [1, 7, 123_457, 3_600_000_001] {
            assert_eq!(Time::from_micros(us).as_micros(), us);
        }
    }

    #[test]
    fn speed_ratio() {
        let d = Time::from_secs(10);
        assert_eq!(d.div_ratio(2, 1), Time::from_secs(5));
        assert_eq!(d.mul_ratio(3, 2), Time::from_secs(15));
    }

    #[test]
    fn range_ops() {
        let a = TimeRange::new(Time::from_secs(0), Time::from_secs(10));
        let b = TimeRange::new(Time::from_secs(5), Time::from_secs(15));
        let c = TimeRange::new(Time::from_secs(10), Time::from_secs(12));
        assert!(a.overlaps(&b));
        assert!(!a.overlaps(&c), "half-open ranges touching do not overlap");
        assert_eq!(a.intersect(&b).unwrap().duration(), Time::from_secs(5));
        assert!(a.contains(Time::ZERO) && !a.contains(Time::from_secs(10)));
    }

    #[test]
    fn serde_is_plain_integer() {
        let json = serde_json::to_string(&Time::from_secs(1)).unwrap();
        assert_eq!(json, "705600000");
    }
}
