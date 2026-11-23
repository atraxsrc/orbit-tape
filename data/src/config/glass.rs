//! See-through ("glass") window background.
//!
//! The window background, and the pane bodies drawn on it, get an opacity
//! below 1 so the desktop shows through, blurred by the compositor when it
//! supports that. Candles, text and lines are drawn as before, so charts stay
//! readable. The opacity is either set by hand or taken from COSMIC's frosted
//! glass setting (see [`CosmicGlass`](super::cosmic::CosmicGlass)).

use serde::{Deserialize, Serialize};

/// Lowest opacity the slider allows; below this text over busy wallpapers
/// gets hard to read.
pub const MIN_OPACITY: f32 = 0.3;

/// Opacity used before the user picks one, close to COSMIC's "Low" frost.
pub const DEFAULT_OPACITY: f32 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Glass {
    /// Off = the window is always opaque.
    pub enabled: bool,
    /// Take the opacity from COSMIC's frosted windows setting. When COSMIC
    /// can't be read, or frosted windows are off there, the window is opaque.
    pub follow_cosmic: bool,
    /// The hand-picked opacity, used when COSMIC isn't followed.
    pub opacity: f32,
}

impl Default for Glass {
    fn default() -> Self {
        Self {
            enabled: true,
            follow_cosmic: true,
            opacity: DEFAULT_OPACITY,
        }
    }
}

impl Glass {
    /// The window background opacity to use, `1.0` = opaque. `cosmic` is the
    /// opacity COSMIC asks for, if it could be read.
    pub fn effective_opacity(self, cosmic: Option<f32>) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let opacity = if self.follow_cosmic {
            cosmic.unwrap_or(1.0)
        } else {
            self.opacity
        };
        clamp_opacity(opacity)
    }
}

/// Keeps an opacity in `MIN_OPACITY..=1.0`; NaN counts as opaque.
pub fn clamp_opacity(opacity: f32) -> f32 {
    if opacity.is_nan() {
        1.0
    } else {
        opacity.clamp(MIN_OPACITY, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_opacity() {
        let glass = Glass::default();
        assert_eq!(glass.effective_opacity(Some(0.87)), 0.87);
        // COSMIC unreadable or frosted windows off there: opaque.
        assert_eq!(glass.effective_opacity(None), 1.0);
        assert_eq!(glass.effective_opacity(Some(1.0)), 1.0);

        let manual = Glass {
            follow_cosmic: false,
            opacity: 0.6,
            ..Glass::default()
        };
        assert_eq!(manual.effective_opacity(Some(0.87)), 0.6);
        assert_eq!(manual.effective_opacity(None), 0.6);

        let off = Glass {
            enabled: false,
            ..manual
        };
        assert_eq!(off.effective_opacity(Some(0.5)), 1.0);
    }

    #[test]
    fn opacity_is_clamped() {
        let manual = |opacity| Glass {
            follow_cosmic: false,
            opacity,
            ..Glass::default()
        };
        assert_eq!(manual(0.0).effective_opacity(None), MIN_OPACITY);
        assert_eq!(manual(1.5).effective_opacity(None), 1.0);
        assert_eq!(manual(f32::NAN).effective_opacity(None), 1.0);
        assert_eq!(Glass::default().effective_opacity(Some(0.1)), MIN_OPACITY);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let parsed: Glass = serde_json::from_str("{\"opacity\": 0.5}").unwrap();
        assert_eq!(
            parsed,
            Glass {
                opacity: 0.5,
                ..Glass::default()
            }
        );
    }
}
