//! Shake detection state machine.
//!
//! Ported from the original Qt implementation: every sample looks at the
//! last three cursor positions. If the angle at the middle point is sharper
//! than 90 degrees the movement reversed -> that is a "corner". Two corners
//! within `gap` and at least `min_dist` apart mean the user is shaking the
//! mouse to find the cursor.

use std::time::{Duration, Instant};

use crate::geometry::{distance, include_angle, Point};

pub const DEFAULT_SLOW_SPEED_PX_S: f64 = 350.0;
pub const DEFAULT_CORNER_ANGLE_DEG: f64 = 90.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Nothing interesting in this sample.
    None,
    /// The cursor changed direction sharply. Call [`ShakeDetector::corner`]
    /// (unless suppressed, e.g. while a full screen app is focused).
    Corner,
    /// The cursor moved slowly in a straight-ish line -> candidate for
    /// hiding the enlarged cursor again.
    Slow,
}

pub struct ShakeDetector {
    /// Max milliseconds allowed between two corners that count as a shake.
    pub gap_ms: u64,
    /// Min pixel distance between two corners that count as a shake.
    pub min_dist: f64,
    /// Speed below which the cursor counts as "slow" (px/s).
    pub slow_speed_px_s: f64,
    /// Angle (degrees) below which a turn counts as a corner.
    pub corner_angle_deg: f64,
    prev_prev: Option<Point>,
    last_movement: Option<Instant>,
    prev: Option<(Point, Instant)>,
    last_corner: Option<(Point, Instant)>,
}

impl ShakeDetector {
    pub fn new(gap_ms: u64, min_dist: f64) -> Self {
        Self {
            gap_ms,
            min_dist,
            slow_speed_px_s: DEFAULT_SLOW_SPEED_PX_S,
            corner_angle_deg: DEFAULT_CORNER_ANGLE_DEG,
            prev_prev: None,
            last_movement: None,
            prev: None,
            last_corner: None,
        }
    }

    /// Feed one cursor position sample. Mirrors the original 20 ms timer tick.
    pub fn sample(&mut self, pos: Point, now: Instant) -> Signal {
        // Preserve moving points through short turn pauses. Repeated samples
        // otherwise erase the incoming segment and lose the next reversal.
        if let Some((prev, _)) = self.prev {
            if self
                .last_movement
                .is_some_and(|last| now.saturating_duration_since(last) > Duration::from_millis(60))
            {
                self.prev_prev = None;
                self.last_corner = None;
            }
            if pos == prev {
                return Signal::Slow;
            }
        }
        let signal = match (self.prev_prev, self.prev) {
            (Some(pp), Some((p, p_time))) => {
                let angle = include_angle(pp, p, pos);
                if angle < self.corner_angle_deg {
                    Signal::Corner
                } else {
                    let dt = now.duration_since(p_time).as_secs_f64();
                    // The original divided by the nominal 20 ms timer
                    // interval; measuring the real elapsed time is more
                    // accurate when a tick arrives late.
                    let speed = if dt > 0.0 {
                        distance(pos, p) / dt
                    } else {
                        f64::MAX
                    };
                    if speed < self.slow_speed_px_s {
                        Signal::Slow
                    } else {
                        Signal::None
                    }
                }
            }
            _ => Signal::None,
        };

        self.last_movement = Some(now);
        self.prev_prev = self.prev.map(|(p, _)| p);
        self.prev = Some((pos, now));
        signal
    }

    /// Register a detected corner; returns true when two recent corners are
    /// close in time and far enough apart, i.e. a shake. Mirrors the
    /// original `cornerDetected` handler (which also always replaces the
    /// stored corner).
    pub fn corner(&mut self, pos: Point, now: Instant) -> bool {
        let shake = match self.last_corner {
            Some((last_pos, last_time)) => {
                let elapsed_ms = now.duration_since(last_time).as_millis() as u64;
                elapsed_ms <= self.gap_ms && distance(last_pos, pos) >= self.min_dist
            }
            None => false,
        };
        self.last_corner = Some((pos, now));
        shake
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn shake_detected_for_two_fast_distant_corners() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        // First corner: reversal at (200, 0)
        d.sample(Point::new(0.0, 0.0), t);
        d.sample(Point::new(200.0, 0.0), t + Duration::from_millis(20));
        assert_eq!(
            d.sample(Point::new(0.0, 0.0), t + Duration::from_millis(40)),
            Signal::Corner
        );
        assert!(!d.corner(Point::new(0.0, 0.0), t + Duration::from_millis(40))); // first corner, no shake yet
                                                                                 // Second corner: reversal at (200, 0), 20 ms later, 200 px away
        assert_eq!(
            d.sample(Point::new(200.0, 0.0), t + Duration::from_millis(60)),
            Signal::Corner
        );
        assert!(d.corner(Point::new(200.0, 0.0), t + Duration::from_millis(60)));
    }

    #[test]
    fn no_shake_when_corners_too_far_apart_in_time() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        d.corner(Point::new(0.0, 0.0), t);
        assert!(!d.corner(Point::new(300.0, 0.0), t + Duration::from_millis(500)));
    }

    #[test]
    fn no_shake_when_corners_too_close_in_distance() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        d.corner(Point::new(0.0, 0.0), t);
        assert!(!d.corner(Point::new(100.0, 0.0), t + Duration::from_millis(50)));
    }

    #[test]
    fn slow_straight_movement_is_slow() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        d.sample(Point::new(0.0, 0.0), t);
        d.sample(Point::new(1.0, 0.0), t + Duration::from_millis(20));
        // 1 px per 20 ms = 50 px/s < 350 -> slow
        assert_eq!(
            d.sample(Point::new(2.0, 0.0), t + Duration::from_millis(40)),
            Signal::Slow
        );
    }

    #[test]
    fn fast_straight_movement_is_neither_corner_nor_slow() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        d.sample(Point::new(0.0, 0.0), t);
        d.sample(Point::new(100.0, 0.0), t + Duration::from_millis(20));
        // 100 px per 20 ms = 5000 px/s -> too fast to be slow, straight line
        assert_eq!(
            d.sample(Point::new(200.0, 0.0), t + Duration::from_millis(40)),
            Signal::None
        );
    }
    #[test]
    fn reversal_survives_a_40ms_pause_at_the_turn() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        for (ms, x) in [(0, 0.0), (20, 200.0), (40, 200.0), (60, 200.0)] {
            d.sample(Point::new(x, 0.0), t + Duration::from_millis(ms));
        }
        assert_eq!(
            d.sample(Point::new(0.0, 0.0), t + Duration::from_millis(80)),
            Signal::Corner
        );
        assert!(!d.corner(Point::new(0.0, 0.0), t + Duration::from_millis(80)));
        for ms in [100, 120] {
            d.sample(Point::new(0.0, 0.0), t + Duration::from_millis(ms));
        }
        assert_eq!(
            d.sample(Point::new(200.0, 0.0), t + Duration::from_millis(140)),
            Signal::Corner
        );
        assert!(d.corner(Point::new(200.0, 0.0), t + Duration::from_millis(140)));
    }
    #[test]
    fn long_stop_does_not_invent_a_reversal_on_restart() {
        let t = t0();
        let mut d = ShakeDetector::new(120, 150.0);
        d.sample(Point::new(0.0, 0.0), t);
        d.sample(Point::new(200.0, 0.0), t + Duration::from_millis(20));
        d.sample(Point::new(200.0, 0.0), t + Duration::from_millis(200));
        assert_ne!(
            d.sample(Point::new(0.0, 0.0), t + Duration::from_millis(220)),
            Signal::Corner
        );
    }
}
