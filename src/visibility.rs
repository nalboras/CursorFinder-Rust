//! Movement-sustained enlargement.
//!
//! A confirmed shake enlarges the cursor. Once enlarged, ANY significant
//! pointer movement keeps it enlarged - no reversal cadence is required, so
//! a steady shake rhythm never flickers at direction changes. 300 ms without
//! significant movement counts as a stop; shrinking then runs over 200 ms,
//! so the cursor is back to normal within 500 ms of the last movement.
//! Only a confirmed shake can start or restart enlargement, so casual
//! movement never grows the cursor.

use std::time::{Duration, Instant};

use crate::{
    detect::DEFAULT_SLOW_SPEED_PX_S,
    geometry::{distance, Point},
};

/// Stillness that ends the gesture. Long enough that pauses at a direction
/// change during steady shaking never read as a stop.
pub const STOP_DELAY: Duration = Duration::from_millis(300);
const GROW: Duration = Duration::from_millis(100);
/// STOP_DELAY + SHRINK = 500 ms: normal size returns within the hard limit
/// after the last significant movement.
const SHRINK: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct ShakeVisibility {
    started: Option<(Instant, f64)>,
    last_motion: Option<Instant>,
    previous: Option<(Point, Instant)>,
}

impl ShakeVisibility {
    fn stop_at(&self) -> Option<Instant> {
        Some(self.last_motion? + STOP_DELAY)
    }

    /// Track movement. Significant movement refreshes the gesture, but only
    /// while it is still alive: once the stop delay elapsed, a new confirmed
    /// shake is required to enlarge again.
    pub fn motion(&mut self, pos: Point, now: Instant) {
        if let Some((prev, time)) = self.previous {
            let dt = now.saturating_duration_since(time).as_secs_f64();
            let moving = dt > 0.0 && distance(prev, pos) / dt >= DEFAULT_SLOW_SPEED_PX_S;
            if moving && self.stop_at().is_some_and(|stop| now <= stop) {
                self.last_motion = Some(now);
            }
        }
        self.previous = Some((pos, now));
    }

    /// A shake was confirmed. Starts growth (from the current level when
    /// resuming mid-shrink) or confirms that the gesture is alive.
    pub fn shake(&mut self, now: Instant) {
        if self.stop_at().is_none_or(|stop| now > stop) {
            let level = self.fraction(now);
            self.started = Some((now, level));
        }
        self.last_motion = Some(now);
    }

    /// 0.0 = normal size, 1.0 = fully enlarged.
    pub fn fraction(&self, now: Instant) -> f64 {
        let (Some((start, level)), Some(stop)) = (self.started, self.stop_at()) else {
            return 0.0;
        };
        let growth_at = |time: Instant| {
            (level + time.saturating_duration_since(start).as_secs_f64() / GROW.as_secs_f64())
                .min(1.0)
        };
        if now <= stop {
            growth_at(now)
        } else {
            // Shrink from the exact level reached at stop, including a stop
            // during growth. Delayed timer messages cannot lengthen the fade.
            (growth_at(stop) - now.duration_since(stop).as_secs_f64() / SHRINK.as_secs_f64())
                .max(0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(t: Instant, ms: u64) -> Instant {
        t + Duration::from_millis(ms)
    }
    fn move_to(v: &mut ShakeVisibility, t: Instant, ms: u64, x: f64) {
        v.motion(Point::new(x, 0.0), at(t, ms));
    }

    #[test]
    fn no_shake_is_hidden() {
        assert_eq!(ShakeVisibility::default().fraction(Instant::now()), 0.0);
    }

    #[test]
    fn casual_motion_without_a_shake_never_enlarges() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        for ms in (0..=2000).step_by(20) {
            move_to(&mut v, t, ms, ms as f64 * 10.0);
            assert_eq!(v.fraction(at(t, ms)), 0.0);
        }
    }

    #[test]
    fn real_stop_shrinks_after_300ms_and_is_gone_by_500ms() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        for ms in (0..=200).step_by(20) {
            move_to(&mut v, t, ms, ms as f64 * 10.0);
        }
        // Fully grown long after the 100 ms growth.
        assert_eq!(v.fraction(at(t, 400)), 1.0);
        // Last significant movement at 200 ms: stop at 500 ms, gone at 700 ms.
        assert_eq!(v.fraction(at(t, 500)), 1.0);
        assert!(v.fraction(at(t, 501)) < 1.0);
        assert!((v.fraction(at(t, 600)) - 0.5).abs() < 1e-9);
        assert_eq!(v.fraction(at(t, 700)), 0.0);
    }

    #[test]
    fn stop_right_after_the_shake_is_gone_by_500ms() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        // No further movement: stop at 300 ms, shrink 200 ms -> 500 ms total.
        assert!(v.fraction(at(t, 300)) > 0.0);
        assert!(v.fraction(at(t, 301)) < 1.0);
        assert_eq!(v.fraction(at(t, 500)), 0.0);
    }

    #[test]
    fn steady_fast_shaking_never_flickers_without_new_confirmations() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        // Fast rhythm: full swing every 100 ms, brief pause at each turn.
        // Only ONE confirmed shake at the start - motion alone must sustain.
        for ms in (20..=5000).step_by(20) {
            let x = match ms % 100 {
                0..=20 => 0.0,
                40 => 100.0,
                60..=80 => 200.0,
                _ => 100.0,
            };
            move_to(&mut v, t, ms, x);
            if ms >= 120 {
                assert_eq!(v.fraction(at(t, ms)), 1.0, "flicker at {ms}ms");
            }
        }
    }

    #[test]
    fn steady_slow_shaking_never_flickers_without_new_confirmations() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        // Slow rhythm: swing every 240 ms with ~80 ms dwell at each end.
        for ms in (20..=5000).step_by(20) {
            let x = match ms % 240 {
                0..=60 => 0.0,
                80..=120 => 200.0,
                140..=200 => 200.0,
                _ => 100.0,
            };
            move_to(&mut v, t, ms, x);
            if ms >= 120 {
                assert_eq!(v.fraction(at(t, ms)), 1.0, "flicker at {ms}ms");
            }
        }
    }

    #[test]
    fn straight_motion_also_sustains_once_enlarged() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        for ms in (20..=2000).step_by(20) {
            move_to(&mut v, t, ms, ms as f64 * 10.0);
            if ms >= 120 {
                assert_eq!(v.fraction(at(t, ms)), 1.0, "flicker at {ms}ms");
            }
        }
        // ...but 300 ms of stillness still ends it, gone by 500 ms.
        assert_eq!(v.fraction(at(t, 2000 + 500)), 0.0);
    }

    #[test]
    fn slow_settling_does_not_sustain() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        for ms in (20..=600).step_by(20) {
            move_to(&mut v, t, ms, ms as f64 * 0.05);
        }
        // 0.05 px per 20 ms is far below the significance threshold, so the
        // gesture ended 300 ms after the shake itself.
        assert_eq!(v.fraction(at(t, 500)), 0.0);
    }

    #[test]
    fn late_ticks_and_maximum_gap_still_finish_under_500ms() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        // A single very late follow-up sample far past the deadline.
        assert_eq!(v.fraction(at(t, 10000)), 0.0);
    }

    #[test]
    fn resuming_during_shrink_grows_smoothly_without_a_jump() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        move_to(&mut v, t, 0, 0.0);
        move_to(&mut v, t, 20, 200.0);
        // Last motion at 20 ms: stop at 320 ms. Level at stop: 1.0
        // (growth finished at 100 ms). At 420 ms: 100 ms into the 200 ms
        // shrink -> 0.5.
        let level = v.fraction(at(t, 420));
        assert!((level - 0.5).abs() < 1e-9);
        v.shake(at(t, 420));
        assert_eq!(v.fraction(at(t, 420)), level);
        assert!((v.fraction(at(t, 470)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn motion_alone_cannot_revive_an_expired_gesture() {
        let t = Instant::now();
        let mut v = ShakeVisibility::default();
        v.shake(t);
        move_to(&mut v, t, 0, 0.0);
        move_to(&mut v, t, 20, 200.0);
        // Gesture expired at 320 ms; fast movement afterwards does nothing.
        move_to(&mut v, t, 800, 5000.0);
        assert_eq!(v.fraction(at(t, 800)), 0.0);
        // A new confirmed shake revives it.
        v.shake(at(t, 800));
        assert!((v.fraction(at(t, 850)) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn detector_and_visibility_steady_shake_no_flicker_real_stop_gone_by_500ms() {
        use crate::detect::{ShakeDetector, Signal};
        let t = Instant::now();
        let mut d = ShakeDetector::new(120, 150.0);
        let mut v = ShakeVisibility::default();
        // 3 s of steady shaking (the rhythm the detector confirms: sharp
        // reversals every 60 ms), then a real stop.
        for ms in (0..=3000).step_by(20) {
            let x = match ms % 120 {
                0..=40 => 0.0,
                _ => 200.0,
            };
            let pos = Point::new(x, 0.0);
            let now = at(t, ms);
            v.motion(pos, now);
            if d.sample(pos, now) == Signal::Corner && d.corner(pos, now) {
                v.shake(now);
            }
            if ms >= 300 {
                assert_eq!(v.fraction(now), 1.0, "flicker at {ms}ms");
            }
        }
        // Real stop: gone within 500 ms of the last moving sample (3000 ms).
        assert_eq!(v.fraction(at(t, 3000 + 500)), 0.0);
    }

    #[test]
    fn visibility_sustains_on_motion_even_when_reversals_stop_confirming() {
        use crate::detect::{ShakeDetector, Signal};
        let t = Instant::now();
        let mut d = ShakeDetector::new(120, 150.0);
        let mut v = ShakeVisibility::default();
        // 1 s of confirming shakes, then the pointer keeps moving at the
        // same rhythm but corners stop being registered (as happens when
        // the dwell at a turn exceeds the detector history window). The
        // v2.0.2 reversal cadence cap faded this out mid-gesture.
        for ms in (0..=4000).step_by(20) {
            let x = match ms % 120 {
                0..=40 => 0.0,
                _ => 200.0,
            };
            let pos = Point::new(x, 0.0);
            let now = at(t, ms);
            v.motion(pos, now);
            if ms < 1000 && d.sample(pos, now) == Signal::Corner && d.corner(pos, now) {
                v.shake(now);
            }
            if ms >= 300 {
                assert_eq!(v.fraction(now), 1.0, "flicker at {ms}ms");
            }
        }
        assert_eq!(v.fraction(at(t, 4000 + 500)), 0.0);
    }
}
