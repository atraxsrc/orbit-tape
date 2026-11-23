//! Alert delivery settings. Which plots are alert conditions is decided by the
//! indicator source (`alert = true`); these settings decide how an alert
//! reaches the user and which conditions are muted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alerts {
    /// Off = alert conditions are ignored.
    pub enabled: bool,
    /// Show a desktop notification.
    pub notification: bool,
    /// Play the alert chime.
    pub sound: bool,
    /// Chime volume, 0-100.
    pub volume: f32,
    /// Conditions turned off, as `indicator-id/plot-id`.
    pub muted: BTreeSet<String>,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            enabled: true,
            notification: true,
            sound: true,
            volume: 60.0,
            muted: BTreeSet::new(),
        }
    }
}

impl Alerts {
    pub fn is_muted(&self, condition_key: &str) -> bool {
        self.muted.contains(condition_key)
    }

    pub fn set_muted(&mut self, condition_key: String, muted: bool) {
        if muted {
            self.muted.insert(condition_key);
        } else {
            self.muted.remove(&condition_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_files_get_defaults() {
        let alerts: Alerts = serde_json::from_str("{}").expect("empty object should load");
        assert_eq!(alerts, Alerts::default());
        let alerts: Alerts =
            serde_json::from_str(r#"{"sound": false, "muted": ["a/b"]}"#).expect("partial");
        assert!(alerts.enabled && alerts.notification && !alerts.sound);
        assert!(alerts.is_muted("a/b"));
    }
}
