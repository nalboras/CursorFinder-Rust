//! The classic Windows arrow cursor as a vector polygon. GDI+ renders it at
//! whatever size the grow/shrink animation needs, so the enlarged cursor is
//! crisp instead of a scaled-up bitmap. A `cursor.*` file next to the exe
//! still overrides this with a custom image.

/// Design-space size of the polygon's bounding box. Proportions match the
/// classic Windows arrow artwork (13 x 20 pixel-art grid).
pub const ARROW_W: f32 = 12.5;
pub const ARROW_H: f32 = 19.5;

/// Bounding box as integers (design units x10) for aspect math.
pub const ARROW_DIMS: (u32, u32) = (125, 195);

/// Outline stroke width in design units.
pub const ARROW_STROKE: f32 = 0.9;

/// Polygon vertices in design units, from the tip (hotspot) clockwise.
pub const ARROW_POINTS: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 17.0),
    (4.5, 13.0),
    (8.0, 19.5),
    (10.5, 17.5),
    (8.0, 12.5),
    (12.5, 12.5),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_stays_inside_its_bounding_box() {
        for (x, y) in ARROW_POINTS {
            assert!((0.0..=ARROW_W).contains(&x), "x={x}");
            assert!((0.0..=ARROW_H).contains(&y), "y={y}");
        }
    }

    #[test]
    fn bounding_box_matches_shipped_artwork_aspect() {
        let shipped = 277.0 / 430.0;
        let ours = ARROW_W / ARROW_H;
        assert!((shipped - ours).abs() < 0.01);
    }

    #[test]
    fn tip_is_the_origin() {
        assert_eq!(ARROW_POINTS[0], (0.0, 0.0));
    }
}
