//! SMPTE timecode display. Timecode is presentation only — the timeline
//! never stores it.

use crate::time::{FrameRate, Time};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    pub negative: bool,
    pub hours: u32,
    pub minutes: u32,
    pub seconds: u32,
    pub frames: u32,
    pub drop_frame: bool,
}

impl Timecode {
    /// Timecode of the frame containing `t`. NTSC rates use drop-frame.
    pub fn from_time(t: Time, rate: FrameRate) -> Self {
        let frame = rate.time_to_frame(t);
        Self::from_frame(frame, rate)
    }

    pub fn from_frame(frame: i64, rate: FrameRate) -> Self {
        let negative = frame < 0;
        let mut f = frame.unsigned_abs();
        let fps = rate.nominal() as u64;
        let drop_frame = rate.is_drop_frame();
        if drop_frame {
            // SMPTE 12M: skip frame numbers 0 and 1 (×2 at 59.94) at the start
            // of every minute except every tenth minute.
            let drop = fps / 15; // 2 for 29.97, 4 for 59.94
            let per_10min = fps * 600 - drop * 9;
            let per_min = fps * 60 - drop;
            let tens = f / per_10min;
            let rem = f % per_10min;
            f += drop * 9 * tens;
            if rem > drop {
                f += drop * ((rem - drop) / per_min);
            }
        }
        Timecode {
            negative,
            hours: (f / (fps * 3600)) as u32,
            minutes: ((f / (fps * 60)) % 60) as u32,
            seconds: ((f / fps) % 60) as u32,
            frames: (f % fps) as u32,
            drop_frame,
        }
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sep = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{}{:02}:{:02}:{:02}{}{:02}",
            if self.negative { "-" } else { "" },
            self.hours,
            self.minutes,
            self.seconds,
            sep,
            self.frames
        )
    }
}

/// Compact human duration: `1h 47m`, `3m 05s`, `12.4s`.
pub fn format_duration(t: Time) -> String {
    let secs = t.as_secs_f64().max(0.0);
    if secs >= 3600.0 {
        let m = (secs / 60.0).floor() as u64;
        format!("{}h {:02}m", m / 60, m % 60)
    } else if secs >= 60.0 {
        let s = secs.floor() as u64;
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{secs:.1}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_drop() {
        let tc = Timecode::from_time(Time::from_millis(3_723_040), FrameRate::FPS_25);
        assert_eq!(tc.to_string(), "01:02:03:01");
    }

    #[test]
    fn drop_frame_minute_boundaries() {
        let r = FrameRate::FPS_29_97;
        assert_eq!(Timecode::from_frame(1799, r).to_string(), "00:00:59;29");
        // Frame 1800 skips ;00 and ;01.
        assert_eq!(Timecode::from_frame(1800, r).to_string(), "00:01:00;02");
        // Tenth minute does not drop.
        assert_eq!(Timecode::from_frame(17982, r).to_string(), "00:10:00;00");
        // One real hour of 29.97 ≈ 01:00:00;00 in drop-frame.
        assert_eq!(Timecode::from_frame(107_892, r).to_string(), "01:00:00;00");
    }

    #[test]
    fn drop_frame_59_94() {
        let r = FrameRate::FPS_59_94;
        assert_eq!(Timecode::from_frame(3600, r).to_string(), "00:01:00;04");
        assert_eq!(Timecode::from_frame(215_784, r).to_string(), "01:00:00;00");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(Time::from_secs(6420)), "1h 47m");
        assert_eq!(format_duration(Time::from_secs(185)), "3m 05s");
        assert_eq!(format_duration(Time::from_millis(12_400)), "12.4s");
    }
}
