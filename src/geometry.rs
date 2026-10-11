//! Geometry helpers shared by the shake detector.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Euclidean distance between two points.
pub fn distance(a: Point, b: Point) -> f64 {
    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
}

/// Angle at B formed by the segment A-B-C, in degrees (0..=180).
///
/// Returns 360.0 for a degenerate segment (two identical points) which
/// mirrors the original Qt implementation: a degenerate angle is never
/// treated as a corner.
pub fn include_angle(a: Point, b: Point, c: Point) -> f64 {
    let side_a = distance(b, c);
    let side_b = distance(a, c);
    let side_c = distance(a, b);

    let denom = 2.0 * side_a * side_c;
    if denom.abs() < 1e-9 {
        return 360.0;
    }
    // Clamp against floating point overshoot (the original used raw qAcos,
    // which can return NaN when the argument drifts just past +/-1).
    let cos = ((side_c * side_c + side_a * side_a - side_b * side_b) / denom).clamp(-1.0, 1.0);
    cos.acos().to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_line_is_not_a_corner() {
        let angle = include_angle(
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(200.0, 0.0),
        );
        assert!((angle - 180.0).abs() < 1e-6);
    }

    #[test]
    fn reversal_is_zero_degrees() {
        let angle = include_angle(
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(0.0, 0.0),
        );
        assert!((angle - 0.0).abs() < 1e-6);
    }

    #[test]
    fn right_angle() {
        let angle = include_angle(
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
        );
        assert!((angle - 90.0).abs() < 1e-6);
    }

    #[test]
    fn degenerate_points_are_not_a_corner() {
        let angle = include_angle(
            Point::new(5.0, 5.0),
            Point::new(5.0, 5.0),
            Point::new(5.0, 5.0),
        );
        assert_eq!(angle, 360.0);
    }

    #[test]
    fn distance_basics() {
        assert_eq!(distance(Point::new(0.0, 0.0), Point::new(3.0, 4.0)), 5.0);
    }
}
