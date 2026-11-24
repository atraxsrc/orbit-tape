use super::ScaleFactor;
use super::alerts::Alerts;
use super::cosmic::ThemeSource;
use super::glass::Glass;
use super::sidebar::Sidebar;
use super::timezone::UserTimezone;
use crate::config::network::Network;
use crate::layout::WindowSpec;
use crate::{AudioStream, Layout, Theme};

use serde::{Deserialize, Serialize};

/// Settings that must exist before the running application state does.
///
/// iced consumes these while the daemon is being constructed, so they are read
/// once before `OrbitTape` exists and never mutated in place. Everything the
/// running app reads and writes belongs in `State` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct BootSettings {
    pub antialiasing: bool,
}

impl Default for BootSettings {
    fn default() -> Self {
        Self { antialiasing: true }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Layouts {
    pub layouts: Vec<Layout>,
    pub active_layout: Option<String>,
    #[serde(default)]
    pub chart_presets: Vec<crate::chart::kline::ChartPreset>,
}

#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct State {
    pub layout_manager: Layouts,
    pub selected_theme: Theme,
    pub custom_theme: Option<Theme>,
    pub theme_source: ThemeSource,
    pub glass: Glass,
    /// Accent colors set by hand in the theme editor, as `#rrggbb` hex. An
    /// empty slot keeps the COSMIC palette's or the generated color.
    pub accent_colors: [Option<String>; super::theme::ACCENT_COUNT],
    pub main_window: Option<WindowSpec>,
    pub timezone: UserTimezone,
    pub sidebar: Sidebar,
    pub scale_factor: ScaleFactor,
    pub audio_cfg: AudioStream,
    pub alerts: Alerts,
    pub network: Network,
    pub size_in_quote_ccy: exchange::SizeUnit,
    pub cache_market_metadata: bool,
    /// Address of the indicator catalog's `catalog.json`; empty means the
    /// `ORBIT_TAPE_CATALOG_URL` variable or the built-in placeholder.
    pub indicator_catalog_url: String,
    #[serde(flatten)]
    pub boot: BootSettings,
}

impl State {
    pub fn from_parts(
        layout_manager: Layouts,
        selected_theme: Theme,
        custom_theme: Option<Theme>,
        theme_source: ThemeSource,
        glass: Glass,
        main_window: Option<WindowSpec>,
        timezone: UserTimezone,
        sidebar: Sidebar,
        scale_factor: ScaleFactor,
        audio_cfg: AudioStream,
        alerts: Alerts,
        network: Network,
        volume_size_unit: exchange::SizeUnit,
        cache_market_metadata: bool,
        boot: BootSettings,
    ) -> Self {
        State {
            layout_manager,
            selected_theme: Theme(selected_theme.0),
            custom_theme: custom_theme.map(|t| Theme(t.0)),
            theme_source,
            glass,
            accent_colors: Default::default(),
            main_window,
            timezone,
            sidebar,
            scale_factor,
            audio_cfg,
            alerts,
            network,
            size_in_quote_ccy: volume_size_unit,
            cache_market_metadata,
            indicator_catalog_url: String::new(),
            boot,
        }
    }
}
