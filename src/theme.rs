use data::config::cosmic::{CosmicGlass, CosmicTheme, ThemeSource, ThemeStamp};
use data::config::glass::Glass;
use data::config::theme::{ACCENT_COUNT, generated_theme_accents};
use iced_core::{Color, Theme};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeAccents {
    colors: [Color; ACCENT_COUNT],
    generation: u64,
}

impl ThemeAccents {
    fn from_theme(theme: &Theme, generation: u64) -> Self {
        Self {
            colors: generated_theme_accents(theme.extended_palette()),
            generation,
        }
    }

    pub fn as_array(&self) -> &[Color; ACCENT_COUNT] {
        &self.colors
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// The app theme: the user's selected theme, optionally overridden by the
/// COSMIC desktop theme, plus the window glass.
#[derive(Debug, Clone)]
pub struct ThemeState {
    /// The built-in or custom theme picked in the settings. This is what gets
    /// persisted, and what is shown when COSMIC isn't followed or unavailable.
    selected: data::Theme,
    source: ThemeSource,
    /// The last COSMIC theme read, if it could be read.
    cosmic: Option<CosmicTheme>,
    /// Modification times of the COSMIC files when `cosmic` was read.
    cosmic_stamp: ThemeStamp,
    /// The theme actually in use.
    effective: Theme,
    accents: ThemeAccents,
    glass: Glass,
    /// COSMIC's frosted glass setting, read while the glass follows it.
    cosmic_glass: Option<CosmicGlass>,
}

impl ThemeState {
    pub fn new(selected: data::Theme, source: ThemeSource, glass: Glass) -> Self {
        let cosmic_stamp = ThemeStamp::read();
        let cosmic = source.follows_cosmic().then(load_cosmic).flatten();
        let effective = effective_theme(&selected, source, cosmic.as_ref());
        apply_radii(source, cosmic.as_ref());
        log::info!(
            "Theme source: {source:?}, COSMIC theme {}",
            match &cosmic {
                Some(theme) if theme.is_dark => "loaded (dark)",
                Some(_) => "loaded (light)",
                None => "not loaded",
            }
        );

        let state = Self {
            accents: ThemeAccents::from_theme(&effective, 0),
            selected,
            source,
            cosmic,
            cosmic_stamp,
            effective,
            glass,
            cosmic_glass: follows_cosmic_glass(glass)
                .then(load_cosmic_glass)
                .flatten(),
        };
        state.apply_glass();
        state
    }

    /// Selects a built-in or custom theme. Returns whether the effective theme
    /// changed.
    pub fn select(&mut self, theme: Theme) -> bool {
        self.selected = data::Theme(theme);
        self.recompute()
    }

    /// Switches between following COSMIC and the selected theme. Returns whether
    /// the effective theme changed.
    pub fn set_source(&mut self, source: ThemeSource) -> bool {
        self.source = source;
        if source.follows_cosmic() {
            self.cosmic_stamp = ThemeStamp::read();
            self.cosmic = load_cosmic();
        }
        self.recompute()
    }

    /// Changes the glass setting; takes effect on the next redraw.
    pub fn set_glass(&mut self, glass: Glass) {
        let newly_follows = follows_cosmic_glass(glass) && !follows_cosmic_glass(self.glass);
        self.glass = glass;
        if newly_follows {
            self.cosmic_glass = load_cosmic_glass();
        }
        self.apply_glass();
    }

    pub fn glass(&self) -> Glass {
        self.glass
    }

    /// COSMIC's frosted glass setting, if the glass follows it and it could be
    /// read.
    pub fn cosmic_glass(&self) -> Option<CosmicGlass> {
        self.cosmic_glass
            .filter(|_| follows_cosmic_glass(self.glass))
    }

    /// Window background opacity in use, `1.0` = opaque. Only Linux gets a
    /// see-through window (see `window::settings`).
    pub fn window_opacity(&self) -> f32 {
        if cfg!(target_os = "linux") {
            self.glass
                .effective_opacity(self.cosmic_glass().map(CosmicGlass::window_opacity))
        } else {
            1.0
        }
    }

    fn apply_glass(&self) {
        crate::style::glass::set(self.window_opacity());
    }

    /// Whether anything is read from COSMIC's config, so it needs polling.
    pub fn polls_cosmic(&self) -> bool {
        self.source.follows_cosmic() || follows_cosmic_glass(self.glass)
    }

    /// Reads the COSMIC theme again if its files changed since the last read.
    /// Returns whether the effective theme changed.
    pub fn poll_cosmic(&mut self) -> bool {
        if !self.polls_cosmic() {
            return false;
        }

        let stamp = ThemeStamp::read();
        if stamp == self.cosmic_stamp {
            return false;
        }
        self.cosmic_stamp = stamp;
        if self.source.follows_cosmic() {
            self.cosmic = load_cosmic();
        }
        if follows_cosmic_glass(self.glass) {
            self.cosmic_glass = load_cosmic_glass();
            self.apply_glass();
        }
        self.recompute()
    }

    fn recompute(&mut self) -> bool {
        apply_radii(self.source, self.cosmic.as_ref());
        let effective = effective_theme(&self.selected, self.source, self.cosmic.as_ref());
        if effective == self.effective {
            return false;
        }

        let generation = self.accents.generation().wrapping_add(1);
        self.accents = ThemeAccents::from_theme(&effective, generation);
        self.effective = effective;
        true
    }

    pub fn current(&self) -> &Theme {
        &self.effective
    }

    /// The selected theme, for persistence.
    pub fn data_theme(&self) -> data::Theme {
        self.selected.clone()
    }

    pub fn source(&self) -> ThemeSource {
        self.source
    }

    /// The COSMIC theme in use, if COSMIC is followed and could be read.
    pub fn active_cosmic(&self) -> Option<&CosmicTheme> {
        self.cosmic
            .as_ref()
            .filter(|_| self.source.follows_cosmic())
    }

    pub fn accents(&self) -> ThemeAccents {
        self.accents
    }
}

fn effective_theme(
    selected: &data::Theme,
    source: ThemeSource,
    cosmic: Option<&CosmicTheme>,
) -> Theme {
    match (source, cosmic) {
        (ThemeSource::Cosmic, Some(cosmic)) => cosmic.to_iced_theme(),
        _ => selected.0.clone(),
    }
}

/// Sets the widget corner radii to COSMIC's when it is followed.
fn apply_radii(source: ThemeSource, cosmic: Option<&CosmicTheme>) {
    let radii = cosmic
        .filter(|_| source.follows_cosmic())
        .map(|theme| (theme.radius_xs, theme.radius_s));
    crate::style::radius::set_cosmic(radii);
}

fn follows_cosmic_glass(glass: Glass) -> bool {
    glass.enabled && glass.follow_cosmic
}

fn load_cosmic_glass() -> Option<CosmicGlass> {
    match CosmicGlass::load() {
        Ok(glass) => {
            log::info!("Read COSMIC frosted glass: {glass:?}");
            Some(glass)
        }
        Err(err) => {
            log::warn!("COSMIC frosted glass setting unavailable, window stays opaque: {err}");
            None
        }
    }
}

fn load_cosmic() -> Option<CosmicTheme> {
    match CosmicTheme::load() {
        Ok(theme) => {
            log::info!(
                "Read COSMIC theme ({})",
                if theme.is_dark { "dark" } else { "light" }
            );
            Some(theme)
        }
        Err(err) => {
            log::warn!("COSMIC theme unavailable, using the selected theme: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosmic() -> CosmicTheme {
        CosmicTheme {
            is_dark: true,
            background: Color::from_rgb(0.1, 0.1, 0.15),
            text: Color::from_rgb(0.9, 0.9, 1.0),
            accent: Color::from_rgb(0.5, 0.6, 1.0),
            success: Color::from_rgb(0.2, 0.8, 0.5),
            warning: Color::from_rgb(0.9, 0.8, 0.3),
            destructive: Color::from_rgb(0.9, 0.3, 0.3),
            radius_xs: 4.0,
            radius_s: 8.0,
        }
    }

    fn manual_glass() -> Glass {
        Glass {
            follow_cosmic: false,
            opacity: 0.6,
            ..Glass::default()
        }
    }

    #[test]
    fn glass_follows_cosmic_only_when_asked() {
        let mut state = ThemeState::new(
            data::Theme(Theme::Nord),
            ThemeSource::Selected,
            manual_glass(),
        );
        assert!(!state.polls_cosmic());
        state.cosmic_glass = Some(CosmicGlass {
            frosted_windows: true,
            alpha: 0.8,
        });
        // A COSMIC reading is ignored while the glass is set by hand.
        assert!(state.cosmic_glass().is_none());
        if cfg!(target_os = "linux") {
            assert_eq!(state.window_opacity(), 0.6);
        }

        state.glass.follow_cosmic = true;
        assert!(state.polls_cosmic());
        if cfg!(target_os = "linux") {
            assert_eq!(state.window_opacity(), 0.8);
        }

        state.glass.enabled = false;
        assert!(!state.polls_cosmic());
        assert_eq!(state.window_opacity(), 1.0);
    }

    #[test]
    fn effective_theme_follows_source() {
        let selected = data::Theme(Theme::Nord);
        let cosmic = cosmic();

        assert_eq!(
            effective_theme(&selected, ThemeSource::Cosmic, Some(&cosmic)),
            cosmic.to_iced_theme()
        );
        assert_eq!(
            effective_theme(&selected, ThemeSource::Selected, Some(&cosmic)),
            Theme::Nord
        );
        // Falls back to the selected theme when COSMIC can't be read.
        assert_eq!(
            effective_theme(&selected, ThemeSource::Cosmic, None),
            Theme::Nord
        );
    }

    #[test]
    fn selecting_a_theme_bumps_the_accent_generation() {
        let mut state = ThemeState::new(
            data::Theme(Theme::Nord),
            ThemeSource::Selected,
            manual_glass(),
        );
        let before = state.accents().generation();

        assert!(!state.select(Theme::Nord));
        assert_eq!(state.accents().generation(), before);

        assert!(state.select(Theme::Dracula));
        assert_eq!(state.current(), &Theme::Dracula);
        assert_eq!(state.data_theme().0, Theme::Dracula);
        assert_ne!(state.accents().generation(), before);
    }

    #[test]
    fn cosmic_theme_is_not_persisted() {
        let mut state = ThemeState::new(
            data::Theme(Theme::Nord),
            ThemeSource::Selected,
            manual_glass(),
        );
        state.source = ThemeSource::Cosmic;
        state.cosmic = Some(cosmic());
        assert!(state.recompute());

        assert_eq!(state.current(), &cosmic().to_iced_theme());
        assert_eq!(state.data_theme().0, Theme::Nord);
        assert!(state.active_cosmic().is_some());
    }
}
