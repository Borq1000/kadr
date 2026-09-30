//! Simulated-clock A/V test (render spec §8): the player's pacing logic
//! (`PlaySchedule` + `pace`) driven for 10 minutes of 29.97 fps with jittery
//! render times, no real sleeping. Every presented frame must be within one
//! frame of the clock.

use kadr_core::{FrameRate, Time};
use kadr_playback::{Pace, PlaySchedule, Step};

/// xorshift64*: deterministic, good enough for jitter.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [lo, hi) microseconds, as `Time`.
    fn micros(&mut self, lo: u64, hi: u64) -> Time {
        Time::from_micros((lo + self.next() % (hi - lo)) as i64)
    }
}

struct Outcome {
    presented: u64,
    dropped: u64,
    frames: i64,
    worst: Time,
}

/// Runs a play of `minutes` against a simulated clock. `step` quantises what
/// the player sees of the clock (an audio clock advances per device
/// callback); `Time(1)` = continuous.
fn simulate(seed: u64, minutes: i64, step: Time) -> Outcome {
    let rate = FrameRate::FPS_29_97;
    let fd = rate.frame_duration();
    let end = Time::from_secs(60 * minutes);
    let mut sched = PlaySchedule::new(Time::ZERO, rate, end);
    let mut rng = Rng(seed);
    let mut now = Time::ZERO; // the real (audio) position
    let seen = |now: Time| Time(now.flicks() / step.flicks() * step.flicks());
    let mut presented = 0u64;
    let mut worst = Time::ZERO;
    loop {
        match sched.poll(seen(now)) {
            Step::End => break,
            // Timed waits oversleep a little.
            Step::Wait(d) => now += d + rng.micros(0, 1500),
            Step::Render { index, time, .. } => {
                // Evaluate + resolve + render: mostly 4–14 ms, sometimes a 2–3 frame spike.
                now += if rng.next().is_multiple_of(150) { rng.micros(67_000, 100_000) } else { rng.micros(4_000, 14_000) };
                loop {
                    match sched.after_render(index, seen(now)) {
                        Pace::Wait(d) => now += d + rng.micros(0, 1500),
                        Pace::Present => {
                            let drift = (time - seen(now)).abs();
                            assert!(drift <= fd, "frame {index} at {time:?} presented at clock {:?}", seen(now));
                            worst = worst.max(drift);
                            presented += 1;
                            now += rng.micros(100, 600); // present
                            break;
                        }
                        Pace::Drop => break,
                    }
                }
            }
        }
    }
    let frames = rate.time_to_frame(end - Time(1)) + 1;
    Outcome { presented, dropped: sched.dropped(), frames, worst }
}

#[test]
fn ten_minutes_at_29_97_stay_within_one_frame_of_the_clock() {
    let o = simulate(0x5EED_1234, 10, Time(1));
    let fd = FrameRate::FPS_29_97.frame_duration();
    assert_eq!(o.frames, 17_983, "frames starting before 10:00");
    assert_eq!(o.presented + o.dropped, o.frames as u64, "every frame is either shown or counted as dropped");
    assert!(o.dropped > 0, "the spikes cost frames and they are counted");
    assert!(o.dropped * 100 < o.frames as u64 * 3, "{} of {} dropped", o.dropped, o.frames);
    assert!(o.worst <= fd);
}

#[test]
fn a_steppy_audio_clock_keeps_the_same_bound() {
    // 10 ms device callbacks: the clock the player sees jumps in 10 ms steps.
    let o = simulate(42, 10, Time::from_millis(10));
    assert_eq!(o.presented + o.dropped, o.frames as u64);
    assert!(o.dropped * 100 < o.frames as u64 * 3, "{} of {} dropped", o.dropped, o.frames);
}

#[test]
fn without_spikes_nothing_is_dropped() {
    let rate = FrameRate::FPS_29_97;
    let mut sched = PlaySchedule::new(Time::from_secs(5), rate, Time::from_secs(65));
    let mut now = Time::from_secs(5);
    let mut rng = Rng(7);
    loop {
        match sched.poll(now) {
            Step::End => break,
            Step::Wait(d) => now += d + rng.micros(0, 1000),
            Step::Render { index, .. } => {
                now += rng.micros(2_000, 12_000);
                while let Pace::Wait(d) = sched.after_render(index, now) {
                    now += d + rng.micros(0, 1000);
                }
            }
        }
    }
    assert_eq!(sched.dropped(), 0);
    assert_eq!(sched.presented(), 1799, "frames starting within 60 s of 29.97 fps");
}
