//! settings.ini read/write.
//!
//! Keeps the original Qt key names (`[General]` group: GapTime, Distance,
//! DetectFullScreen) so an existing settings.ini next to the old exe keeps
//! working with this rewrite.

/// Minimum/maximum accepted values (same as the original settings dialog).
pub const GAP_MS_RANGE: (i64, i64) = (20, 800);
pub const DIST_PX_RANGE: (i64, i64) = (10, 2000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Max time (ms) between two corners that still count as a shake.
    pub gap_ms: i64,
    /// Min distance (px) between two corners that count as a shake.
    pub dist_px: i64,
    /// Do not trigger while a full screen window is in the foreground.
    pub disable_when_fullscreen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gap_ms: 120,
            dist_px: 150,
            disable_when_fullscreen: true,
        }
    }
}

impl Settings {
    /// Lenient parse: any `key=value` line in any section, unknown keys and
    /// comments ignored, missing keys keep their defaults.
    pub fn parse(content: &str) -> Self {
        let mut s = Settings::default();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with(';')
                || line.starts_with('#')
                || line.starts_with('[')
            {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key.eq_ignore_ascii_case("GapTime") {
                if let Ok(v) = value.parse() {
                    s.gap_ms = v;
                }
            } else if key.eq_ignore_ascii_case("Distance") {
                if let Ok(v) = value.parse() {
                    s.dist_px = v;
                }
            } else if key.eq_ignore_ascii_case("DetectFullScreen") {
                s.disable_when_fullscreen = value.eq_ignore_ascii_case("true") || value == "1";
            }
        }
        s.clamp();
        s
    }

    /// Qt-compatible ini text.
    pub fn serialize(&self) -> String {
        format!(
            "[General]\nGapTime={}\nDistance={}\nDetectFullScreen={}\n",
            self.gap_ms,
            self.dist_px,
            if self.disable_when_fullscreen {
                "true"
            } else {
                "false"
            }
        )
    }

    /// Clamp into the same ranges the original settings dialog enforced.
    pub fn clamp(&mut self) {
        self.gap_ms = self.gap_ms.clamp(GAP_MS_RANGE.0, GAP_MS_RANGE.1);
        self.dist_px = self.dist_px.clamp(DIST_PX_RANGE.0, DIST_PX_RANGE.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qt_style_ini() {
        let ini = "[General]\nGapTime=200\nDistance=300\nDetectFullScreen=false\n";
        let s = Settings::parse(ini);
        assert_eq!(s.gap_ms, 200);
        assert_eq!(s.dist_px, 300);
        assert!(!s.disable_when_fullscreen);
    }

    #[test]
    fn missing_keys_keep_defaults() {
        let s = Settings::parse("[General]\nGapTime=300\n");
        assert_eq!(s.gap_ms, 300);
        assert_eq!(s.dist_px, 150);
        assert!(s.disable_when_fullscreen);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let s = Settings::parse("GapTime=99999\nDistance=1\n");
        assert_eq!(s.gap_ms, 800);
        assert_eq!(s.dist_px, 10);
    }

    #[test]
    fn roundtrip() {
        let s = Settings {
            gap_ms: 250,
            dist_px: 400,
            disable_when_fullscreen: false,
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn garbage_input_yields_defaults() {
        assert_eq!(Settings::parse("not an ini at all"), Settings::default());
    }
}
