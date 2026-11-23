use data::config::cosmic::{CosmicTheme, ThemeSource, ThemeStamp};
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
/// COSMIC desktop theme.
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
}

impl ThemeState {
    pub fn new(selected: data::Theme, source: ThemeSource) -> Self {
        let cosmic_stamp = ThemeStamp::read();
        let cosmic = source.follows_cosmic().then(load_cosmic).flatten();
        let effective = effective_theme(&selected, source, cosmic.as_ref());
        apply_radii(source, cosmic.as_ref());

        Self {
            accents: ThemeAccents::from_theme(&effective, 0),
            selected,
            source,
            cosmic,
            cosmic_stamp,
            effective,
        }
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

    /// Reads the COSMIC theme again if its files changed since the last read.
    /// Returns whether the effective theme changed.
    pub fn poll_cosmic(&mut self) -> bool {
        if !self.source.follows_cosmic() {
            return false;
        }

        let stamp = ThemeStamp::read();
        if stamp == self.cosmic_stamp {
            return false;
        }
        self.cosmic_stamp = stamp;
        self.cosmic = load_cosmic();
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

fn load_cosmic() -> Option<CosmicTheme> {
    match CosmicTheme::load() {
        Ok(theme) => Some(theme),
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
        let mut state = ThemeState::new(data::Theme(Theme::Nord), ThemeSource::Selected);
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
        let mut state = ThemeState::new(data::Theme(Theme::Nord), ThemeSource::Selected);
        state.source = ThemeSource::Cosmic;
        state.cosmic = Some(cosmic());
        assert!(state.recompute());

        assert_eq!(state.current(), &cosmic().to_iced_theme());
        assert_eq!(state.data_theme().0, Theme::Nord);
        assert!(state.active_cosmic().is_some());
    }
}
