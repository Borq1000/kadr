//! Playback timing (render spec §8): which frame to show when, against a
//! master clock. Pure functions and a small state machine, so the same
//! code the player runs is tested on a simulated clock.

use kadr_core::{FrameRate, Time};
use parking_lot::Mutex;
use std::time::{Duration, Instant};

/// Playback master clock: the timeline time being heard now (audio clock in
/// the app; [`WallClock`] without audio). `None` while it is not running.
/// Read on the player thread.
pub trait Clock: Send + Sync {
    fn now(&self) -> Option<Time>;
}

/// A clock running at wall speed from a start time.
#[derive(Default)]
pub struct WallClock {
    start: Mutex<Option<(Instant, Time)>>,
}

impl WallClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs from `at`, now.
    pub fn start(&self, at: Time) {
        *self.start.lock() = Some((Instant::now(), at));
    }

    pub fn stop(&self) {
        *self.start.lock() = None;
    }
}

impl Clock for WallClock {
    fn now(&self) -> Option<Time> {
        self.start.lock().map(|(i, at)| at + time_of(i.elapsed()))
    }
}

/// Wall duration → `Time` (boundary conversion, never accumulated).
pub fn time_of(d: Duration) -> Time {
    Time::from_micros(d.as_micros().min(i64::MAX as u128) as i64)
}

/// `Time` → wall duration; negative → zero.
pub fn duration_of(t: Time) -> Duration {
    Duration::from_micros(t.as_micros().max(0) as u64)
}

/// How early a frame may be presented: it takes the display a moment to
/// pick it up. Never more than a quarter frame.
pub fn present_lead(frame_duration: Time) -> Time {
    Time::from_millis(2).min(Time(frame_duration.flicks() / 4))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// Too early: wait this long (clock time), then ask again.
    Wait(Time),
    Present,
    /// Too late: its whole frame period has passed.
    Drop,
}

/// Whether a frame for timeline time `frame_time` should be shown at clock
/// time `now`: from `frame_time − lead` until the next frame is due. A
/// presented frame is therefore never more than one frame from the clock
/// (the spec's A/V bound); the legacy preview tolerated two.
pub fn pace(now: Time, frame_time: Time, frame_duration: Time) -> Pace {
    let lead = present_lead(frame_duration);
    if now < frame_time - lead {
        Pace::Wait(frame_time - lead - now)
    } else if now < frame_time + frame_duration {
        Pace::Present
    } else {
        Pace::Drop
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Render frame `index` (timeline time `time`) now; `skipped` frames
    /// before it were too late to render at all (count them as dropped).
    Render { index: i64, time: Time, skipped: i64 },
    /// Nothing to do for this long (clock time).
    Wait(Time),
    /// Past the end.
    End,
}

/// The frames of one play: frame `n` of a play from `from` is timeline time
/// `from + rate.frame_to_time(n)` (exact, never accumulated). A frame is
/// rendered up to one frame early and presented on time; when the clock is
/// ahead, the schedule jumps to the frame for the clock instead of
/// rendering every missed one.
#[derive(Clone, Debug)]
pub struct PlaySchedule {
    from: Time,
    rate: FrameRate,
    end: Time,
    next: i64,
    dropped: u64,
    presented: u64,
}

impl PlaySchedule {
    pub fn new(from: Time, rate: FrameRate, end: Time) -> Self {
        PlaySchedule { from, rate, end, next: 0, dropped: 0, presented: 0 }
    }

    pub fn frame_duration(&self) -> Time {
        self.rate.frame_duration()
    }

    pub fn time_of(&self, index: i64) -> Time {
        self.from + self.rate.frame_to_time(index)
    }

    /// Frames dropped so far (skipped or late after rendering).
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn presented(&self) -> u64 {
        self.presented
    }

    pub fn poll(&mut self, now: Time) -> Step {
        let fd = self.frame_duration();
        let mut skipped = 0;
        loop {
            let t = self.time_of(self.next);
            if t >= self.end {
                return Step::End;
            }
            if now < t - fd {
                return Step::Wait(t - fd - now);
            }
            if now < t + fd {
                return Step::Render { index: self.next, time: t, skipped };
            }
            // Behind: jump to the frame the clock is in.
            let mut k = self.rate.time_to_frame(now - self.from).max(self.next + 1);
            while self.time_of(k) + fd <= now {
                k += 1;
            }
            skipped += k - self.next;
            self.dropped += (k - self.next) as u64;
            self.next = k;
        }
    }

    /// After rendering frame `index`: present, wait, or drop it.
    pub fn after_render(&mut self, index: i64, now: Time) -> Pace {
        let p = pace(now, self.time_of(index), self.frame_duration());
        match p {
            Pace::Present => {
                self.presented += 1;
                self.next = index + 1;
            }
            Pace::Drop => self.drop_frame(index),
            Pace::Wait(_) => {}
        }
        p
    }

    /// Frame `index` will not be shown (late, or its inputs were not ready).
    pub fn drop_frame(&mut self, index: i64) {
        self.dropped += 1;
        self.next = self.next.max(index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FD: Time = Time(705_600_000 / 25);

    #[test]
    fn pace_waits_presents_and_drops() {
        let t = Time::from_secs(10);
        assert_eq!(pace(t - Time::from_millis(10), t, FD), Pace::Wait(Time::from_millis(8)));
        assert_eq!(pace(t - Time::from_millis(2), t, FD), Pace::Present, "within the lead");
        assert_eq!(pace(t + FD - Time(1), t, FD), Pace::Present);
        assert_eq!(pace(t + FD, t, FD), Pace::Drop);
        assert_eq!(present_lead(Time::from_millis(4)), Time::from_millis(1), "a quarter frame at most");
    }

    #[test]
    fn schedule_times_are_exact_and_it_skips_to_the_clock() {
        let r = FrameRate::FPS_29_97;
        let from = Time::from_millis(1234);
        let mut s = PlaySchedule::new(from, r, Time::from_secs(3600));
        assert_eq!(s.time_of(30000), from + Time::from_secs(1001), "30000 frames of 30000/1001 = 1001 s exactly");
        assert_eq!(s.poll(from - Time::from_secs(1)), Step::Wait(Time::from_secs(1) - r.frame_duration()));
        assert_eq!(s.poll(from), Step::Render { index: 0, time: from, skipped: 0 });
        assert_eq!(s.after_render(0, from), Pace::Present);
        // The clock jumps 10.5 frames ahead: frames 1..=9 are skipped, 10 is rendered.
        let now = s.time_of(10) + Time(r.frame_duration().flicks() / 2);
        assert_eq!(s.poll(now), Step::Render { index: 10, time: s.time_of(10), skipped: 9 });
        assert_eq!(s.dropped(), 9);
        // Rendering took two frames: dropped, and the next poll moves on.
        assert_eq!(s.after_render(10, s.time_of(12)), Pace::Drop);
        assert!(matches!(s.poll(s.time_of(12)), Step::Render { index: 12, skipped: 1, .. }));
        assert_eq!(s.dropped(), 11);
    }

    #[test]
    fn schedule_ends_at_the_end() {
        let mut s = PlaySchedule::new(Time::ZERO, FrameRate::FPS_25, FD + FD);
        assert!(matches!(s.poll(Time::ZERO), Step::Render { index: 0, .. }));
        s.after_render(0, Time::ZERO);
        assert!(matches!(s.poll(FD), Step::Render { index: 1, .. }));
        s.after_render(1, FD);
        assert_eq!(s.poll(FD + FD), Step::End);
    }
}
