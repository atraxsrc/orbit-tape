use std::borrow::Cow;

use crate::{
    Deferred, screen::sidebar, split_column, style, theme::ThemeState, version, widget::tooltip,
};

use data::{
    config::alert_store::{DEFAULT_NTFY_SERVER, NtfySettings},
    config::alerts::Alerts,
    config::cosmic::ThemeSource,
    config::glass::{self, Glass},
    config::theme::default_theme,
    sidebar::{Menu, Position as SidebarPosition},
};
use iced::{
    Alignment, Element,
    widget::tooltip::Position,
    widget::{button, checkbox, column, container, pick_list, row, scrollable, slider, text},
};

#[derive(Debug, Clone)]
pub enum Message {
    ThemeSelected(iced::Theme),
    SetFollowCosmic(bool),
    /// Opens the theme editor on one accent color.
    EditAccent(usize),
    Glass(GlassChange),
    TimezoneChanged(data::UserTimezone),
    SetMetadataCaching(bool),
    RefreshMetadata,
    SetVolumeSizeUnit(exchange::SizeUnit),
    SetAntialiasing(bool),
    ScaleFactorChanged(data::ScaleFactor),
    OpenDataFolder,
    OpenUrl(Cow<'static, str>),
    Sidebar(sidebar::Message),
    Alert(AlertChange),
    CatalogUrlChanged(String),
}

/// An edit to the alert settings.
#[derive(Debug, Clone)]
pub enum AlertChange {
    Enabled(bool),
    Notification(bool),
    Sound(bool),
    Volume(f32),
    /// Turn one condition (`indicator-id/plot-id`) on or off.
    Condition(String, bool),
    Test,
    /// Also send chart alerts to ntfy.
    ChartNtfy(bool),
    NtfyTopic(String),
    NtfyServer(String),
    SaveNtfy,
    TestNtfy,
}

/// Work the settings cannot do themselves, because it acts on state owned
/// elsewhere. Mirrors `tickers_table::Action`.
pub enum Action {
    /// A restart-gated setting changed, so what is on disk is now stale.
    Persist,
    ThemeSelected(iced::Theme),
    SetThemeSource(ThemeSource),
    EditAccent(usize),
    Glass(GlassChange),
    SetMetadataCaching(bool),
    RefreshMetadata,
    OpenDataFolder,
    OpenUrl(Cow<'static, str>),
    Sidebar(sidebar::Message),
    /// Send a sample alert with the current settings.
    TestAlert,
    /// Save the ntfy topic and server in alerts.json.
    SaveNtfy(NtfySettings),
    /// Send a sample message to the saved ntfy topic.
    TestNtfy,
}

/// An edit to the glass setting, which lives in the theme state.
#[derive(Debug, Clone, Copy)]
pub enum GlassChange {
    Enabled(bool),
    FollowCosmic(bool),
    Opacity(f32),
}

impl GlassChange {
    pub fn apply(self, glass: &mut Glass) {
        match self {
            Self::Enabled(enabled) => glass.enabled = enabled,
            Self::FollowCosmic(follow) => glass.follow_cosmic = follow,
            Self::Opacity(opacity) => glass.opacity = glass::clamp_opacity(opacity),
        }
    }
}

/// The settings the app owns and edits from the settings modal.
pub struct Settings {
    pub timezone: data::UserTimezone,
    pub scale_factor: data::ScaleFactor,
    pub volume_size_unit: Deferred<exchange::SizeUnit>,
    pub boot: Deferred<data::BootSettings>,
    pub alerts: Alerts,
    /// The ntfy fields as typed, and as saved in alerts.json.
    pub ntfy: NtfySettings,
    pub ntfy_saved: NtfySettings,
    /// Saved catalog address; empty means the default (see
    /// `manifest::resolve_catalog_url`).
    pub indicator_catalog_url: String,
}

impl Settings {
    pub fn new(
        timezone: data::UserTimezone,
        scale_factor: data::ScaleFactor,
        volume_size_unit: exchange::SizeUnit,
        boot: data::BootSettings,
        alerts: Alerts,
        ntfy: NtfySettings,
        indicator_catalog_url: String,
    ) -> Self {
        Self {
            timezone,
            scale_factor,
            volume_size_unit: Deferred::new(volume_size_unit),
            boot: Deferred::new(boot),
            alerts,
            ntfy_saved: ntfy.clone(),
            ntfy,
            indicator_catalog_url,
        }
    }

    pub fn update(&mut self, message: Message) -> Option<Action> {
        match message {
            Message::TimezoneChanged(timezone) => {
                self.timezone = timezone;
            }
            Message::SetVolumeSizeUnit(unit) => {
                self.volume_size_unit.set_wanted(unit);
                return Some(Action::Persist);
            }
            Message::SetAntialiasing(enabled) => {
                self.boot.set_wanted(data::BootSettings {
                    antialiasing: enabled,
                });
                return Some(Action::Persist);
            }
            Message::ScaleFactorChanged(value) => {
                self.scale_factor = value;
            }
            Message::ThemeSelected(theme) => return Some(Action::ThemeSelected(theme)),
            Message::EditAccent(index) => return Some(Action::EditAccent(index)),
            Message::SetFollowCosmic(follow) => {
                return Some(Action::SetThemeSource(if follow {
                    ThemeSource::Cosmic
                } else {
                    ThemeSource::Selected
                }));
            }
            Message::Glass(change) => return Some(Action::Glass(change)),
            Message::SetMetadataCaching(enabled) => {
                return Some(Action::SetMetadataCaching(enabled));
            }
            Message::RefreshMetadata => return Some(Action::RefreshMetadata),
            Message::OpenDataFolder => return Some(Action::OpenDataFolder),
            Message::OpenUrl(url) => return Some(Action::OpenUrl(url)),
            Message::Sidebar(message) => return Some(Action::Sidebar(message)),
            Message::CatalogUrlChanged(url) => self.indicator_catalog_url = url,
            Message::Alert(change) => match change {
                AlertChange::Enabled(on) => self.alerts.enabled = on,
                AlertChange::Notification(on) => self.alerts.notification = on,
                AlertChange::Sound(on) => self.alerts.sound = on,
                AlertChange::Volume(volume) => self.alerts.volume = volume.clamp(0.0, 100.0),
                AlertChange::Condition(key, on) => self.alerts.set_muted(key, !on),
                AlertChange::Test => return Some(Action::TestAlert),
                AlertChange::ChartNtfy(on) => self.alerts.ntfy = on,
                AlertChange::NtfyTopic(topic) => self.ntfy.topic = topic,
                AlertChange::NtfyServer(server) => self.ntfy.server = server,
                AlertChange::SaveNtfy => {
                    self.ntfy.topic = self.ntfy.topic.trim().to_owned();
                    self.ntfy.server = self.ntfy.server.trim().to_owned();
                    return Some(Action::SaveNtfy(self.ntfy.clone()));
                }
                AlertChange::TestNtfy => return Some(Action::TestNtfy),
            },
        }
        None
    }

    pub fn view<'a>(
        &'a self,
        theme: &'a ThemeState,
        custom_theme: Option<&'a iced::Theme>,
        sidebar: &'a sidebar::Sidebar,
        sidebar_position: SidebarPosition,
    ) -> Element<'a, Message> {
        let theme_picklist = {
            let mut themes: Vec<iced::Theme> = iced_core::Theme::ALL.to_vec();

            let default_theme = iced_core::Theme::Custom(default_theme().into());
            themes.push(default_theme);

            if let Some(custom_theme) = custom_theme {
                themes.push(custom_theme.clone());
            }

            pick_list(
                themes,
                Some(theme.current().clone()),
                Message::ThemeSelected,
            )
        };

        let follow_cosmic = {
            let follows = theme.source().follows_cosmic();
            let checkbox = tooltip(
                checkbox(follows)
                    .label("Follow COSMIC theme")
                    .on_toggle(Message::SetFollowCosmic),
                Some(
                    "Use the COSMIC desktop's colors and light/dark mode.\n- Picking a theme below turns this off.",
                ),
                Position::Top,
            );

            let mut col = column![row![checkbox].height(24).align_y(Alignment::Center)];
            if follows && theme.active_cosmic().is_none() {
                col = col.push(
                    text("COSMIC theme not found, using the selected theme")
                        .size(crate::style::text_size::SMALL)
                        .style(style::secondary_text),
                );
            }
            col.spacing(4)
        };

        let glass_section = glass_view(theme);

        let accent_section = column![
            text("Accent colors").size(crate::style::text_size::SMALL),
            tooltip(
                crate::modal::theme_editor::accent_squares(
                    theme,
                    crate::modal::theme_editor::Component::Background,
                    Message::EditAccent,
                ),
                Some(
                    "Colors for indicators and drawings.\n- Click a square to set any color, by picker or hex.\n- Colors set by hand stay when the theme or COSMIC palette changes.",
                ),
                Position::Top,
            ),
        ]
        .spacing(4);

        let toggle_theme_editor = button(text("Theme editor")).on_press(Message::Sidebar(
            sidebar::Message::ToggleSidebarMenu(Some(Menu::ThemeEditor)),
        ));

        let toggle_network_editor = button(text("Network")).on_press(Message::Sidebar(
            sidebar::Message::ToggleSidebarMenu(Some(Menu::Network)),
        ));

        let timezone_picklist = pick_list(
            [data::UserTimezone::Utc, data::UserTimezone::Local],
            Some(self.timezone),
            Message::TimezoneChanged,
        );

        let restart_required_marker = || {
            let btn = button(
                text("!").style(|theme: &iced::Theme| iced::widget::text::Style {
                    color: Some(theme.extended_palette().warning.base.color),
                }),
            )
            .style(move |theme, status| style::button::modifier(theme, status, true));
            tooltip(btn, Some("Restart required"), Position::Top)
        };

        let size_in_quote_ccy_checkbox = {
            let is_active = *self.volume_size_unit.wanted() == exchange::SizeUnit::Quote;
            let restart_required = self.volume_size_unit.restart_required();

            let checkbox = checkbox(is_active)
                .label("Size in quote currency")
                .on_toggle(|checked| {
                    Message::SetVolumeSizeUnit(if checked {
                        exchange::SizeUnit::Quote
                    } else {
                        exchange::SizeUnit::Base
                    })
                });

            let checkbox = tooltip(
                checkbox,
                Some(
                    "Display sizes/volumes in quote currency (USD).\n- Has no effect on inverse perps or open interest\n- Restart required to apply.",
                ),
                Position::Top,
            );

            let mut row = row![checkbox]
                .align_y(Alignment::Center)
                .spacing(8)
                .height(24);
            if restart_required {
                row = row.push(restart_required_marker());
            }
            row
        };

        let cache_metadata_checkbox = {
            let cache_enabled = sidebar.cache_enabled();

            let checkbox = tooltip(
                checkbox(cache_enabled)
                    .label("Cache ticker metadata")
                    .on_toggle(Message::SetMetadataCaching),
                Some("Cache ticker metadata for faster startup."),
                Position::Top,
            );

            if !cache_enabled {
                column![row![checkbox].height(24).align_y(Alignment::Center)]
            } else {
                let loading = sidebar.is_metadata_loading();

                let refresh_btn = {
                    let btn = button(crate::style::icon_text(crate::style::Icon::Refresh, 12))
                        .style(move |theme, status| {
                            style::button::modifier(theme, status, loading)
                        });
                    let btn = if loading {
                        btn
                    } else {
                        btn.on_press(Message::RefreshMetadata)
                    };

                    tooltip(
                        btn,
                        if loading { None } else { Some("Refresh now") },
                        Position::Top,
                    )
                };

                let last_updated = {
                    let last_update = sidebar.last_metadata_update();

                    let label = if loading {
                        "Refreshing metadata…".to_string()
                    } else {
                        match last_update {
                            Some(at) => {
                                format!(
                                    "Metadata last update: {}",
                                    data::util::relative_time_label(at)
                                )
                            }
                            None => "Metadata not fetched yet".to_string(),
                        }
                    };

                    text(label)
                        .size(crate::style::text_size::SMALL)
                        .style(style::secondary_text)
                };

                column![
                    row![checkbox, refresh_btn]
                        .align_y(Alignment::Center)
                        .spacing(8),
                    last_updated,
                ]
                .spacing(4)
            }
        };

        let sidebar_pos_picklist = pick_list(
            [SidebarPosition::Left, SidebarPosition::Right],
            Some(sidebar_position),
            |pos| Message::Sidebar(sidebar::Message::SetSidebarPosition(pos)),
        );

        let scale_factor = {
            let current_value: f32 = self.scale_factor.into();

            let decrease_btn = if current_value > data::config::MIN_SCALE {
                button(text("-"))
                    .on_press(Message::ScaleFactorChanged((current_value - 0.1).into()))
            } else {
                button(text("-"))
            };

            let increase_btn = if current_value < data::config::MAX_SCALE {
                button(text("+"))
                    .on_press(Message::ScaleFactorChanged((current_value + 0.1).into()))
            } else {
                button(text("+"))
            };

            container(
                row![
                    decrease_btn,
                    text(format!("{:.0}%", current_value * 100.0))
                        .size(crate::style::text_size::SECTION),
                    increase_btn,
                ]
                .align_y(Alignment::Center)
                .spacing(8)
                .padding(4),
            )
            .style(style::modal_container)
        };

        let antialiasing_checkbox = {
            let restart_required = self.boot.differs(|boot| &boot.antialiasing);
            let checkbox = checkbox(self.boot.wanted().antialiasing)
                .label("Antialiasing")
                .on_toggle(Message::SetAntialiasing);

            let checkbox = tooltip(
                checkbox,
                Some(
                    "Disabling may increase performance, but expect rough edges\n- Restart required to apply.",
                ),
                Position::Top,
            );

            let mut row = row![checkbox]
                .align_y(Alignment::Center)
                .spacing(8)
                .height(24);
            if restart_required {
                row = row.push(restart_required_marker());
            }
            row
        };

        let open_data_folder = {
            let button = button(text("Open data folder")).on_press(Message::OpenDataFolder);

            tooltip(
                button,
                Some("Open the folder where the data & config is stored"),
                Position::Top,
            )
        };

        let version_info = {
            let (version_label, commit_label) = version::app_build_version_parts();

            let github_link_button =
                button(text(version_label).size(crate::style::text_size::EMPHASIS))
                    .padding(0)
                    .style(style::button::text_link)
                    .on_press(Message::OpenUrl(Cow::Borrowed(
                        version::GITHUB_REPOSITORY_URL,
                    )));

            let github_button: Element<'_, Message> = iced::widget::tooltip(
                github_link_button,
                container(
                    row![
                        text("GitHub"),
                        style::icon_text(style::Icon::ExternalLink, 12),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .style(style::tooltip)
                .padding(8),
                Position::Top,
            )
            .into();

            if let (Some(commit_label), Some(commit_url)) =
                (commit_label, version::build_commit_url())
            {
                let commit_button = button(text(commit_label).size(crate::style::text_size::SMALL))
                    .padding(0)
                    .style(style::button::text_link_secondary)
                    .on_press(Message::OpenUrl(Cow::Owned(commit_url)));

                column![github_button, commit_button]
                    .spacing(2)
                    .align_x(Alignment::End)
                    .into()
            } else {
                github_button
            }
        };

        let attribution = {
            let upstream = button(
                text(format!("Based on {}", version::UPSTREAM_NAME))
                    .size(crate::style::text_size::SMALL),
            )
            .padding(0)
            .style(style::button::text_link_secondary)
            .on_press(Message::OpenUrl(Cow::Borrowed(version::UPSTREAM_URL)));
            column![
                text("Orbit Tape").size(crate::style::text_size::EMPHASIS),
                upstream,
                text("GPL-3.0-or-later")
                    .size(crate::style::text_size::SMALL)
                    .style(style::secondary_text),
            ]
            .spacing(2)
            .align_x(Alignment::End)
        };

        let footer = column![
            container(version_info)
                .width(iced::Length::Fill)
                .align_x(Alignment::End),
            container(attribution)
                .width(iced::Length::Fill)
                .align_x(Alignment::End),
        ]
        .spacing(8);

        let column_content = split_column![
            column![open_data_folder,].spacing(8),
            column![text("Sidebar position").size(crate::style::text_size::SECTION), sidebar_pos_picklist,].spacing(12),
            column![text("Time zone").size(crate::style::text_size::SECTION), timezone_picklist,].spacing(12),
            column![text("Market data").size(crate::style::text_size::SECTION), column![size_in_quote_ccy_checkbox, cache_metadata_checkbox].spacing(4)].spacing(12),
            column![text("Theme").size(crate::style::text_size::SECTION), follow_cosmic, theme_picklist, accent_section, glass_section,].spacing(12),
            column![text("Alerts").size(crate::style::text_size::SECTION), alerts_view(&self.alerts), ntfy_view(&self.alerts, &self.ntfy, &self.ntfy_saved),].spacing(12),
            column![text("Indicator catalog").size(crate::style::text_size::SECTION), catalog_url_view(&self.indicator_catalog_url),].spacing(12),
            column![text("Interface scale").size(crate::style::text_size::SECTION), scale_factor,].spacing(12),
            column![text("Rendering").size(crate::style::text_size::SECTION), antialiasing_checkbox,].spacing(12),
            column![
                text("Experimental").size(crate::style::text_size::SECTION),
                column![toggle_theme_editor, toggle_network_editor].spacing(8),
            ]
            .spacing(12),
            footer,
            ; spacing = 16, align_x = Alignment::Start
        ];

        let content = scrollable::Scrollable::with_direction(
            column_content,
            scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(8)
                    .scroller_width(6)
                    .spacing(8),
            ),
        );

        // Wide enough for the longest rows (accent squares, alert names,
        // sliders with their value) so nothing is cut at the right edge.
        container(content)
            .align_x(Alignment::Start)
            .max_width(360)
            .padding(24)
            .style(style::dashboard_modal)
            .into()
    }
}

/// The glass controls: on/off, follow COSMIC's frosted glass, or a hand-set
/// opacity.
fn glass_view(theme: &ThemeState) -> Element<'_, Message> {
    let glass = theme.glass();

    let enabled = tooltip(
        checkbox(glass.enabled)
            .label("Glass window")
            .on_toggle(|on| Message::Glass(GlassChange::Enabled(on))),
        Some(
            "See-through window background, blurred by the desktop.\n- Candles, text and lines stay opaque.",
        ),
        Position::Top,
    );
    let mut col = column![row![enabled].height(24).align_y(Alignment::Center)].spacing(4);
    if !glass.enabled {
        return col.into();
    }

    let follow = tooltip(
        checkbox(glass.follow_cosmic)
            .label("Use COSMIC frosted glass")
            .on_toggle(|on| Message::Glass(GlassChange::FollowCosmic(on))),
        Some("Take the opacity from COSMIC Settings > Appearance > Frosted glass."),
        Position::Top,
    );
    col = col.push(row![follow].height(24).align_y(Alignment::Center));

    if glass.follow_cosmic {
        let note = match theme.cosmic_glass() {
            Some(cosmic) if cosmic.frosted_windows => {
                format!(
                    "COSMIC frosted windows: {:.0}% opacity",
                    cosmic.alpha * 100.0
                )
            }
            Some(_) => "Frosted windows are off in COSMIC, the window stays opaque".to_owned(),
            None => "COSMIC frosted glass not found, the window stays opaque".to_owned(),
        };
        col = col.push(
            text(note)
                .size(crate::style::text_size::SMALL)
                .style(style::secondary_text),
        );
    } else {
        let opacity = slider(glass::MIN_OPACITY..=1.0, glass.opacity, |value| {
            Message::Glass(GlassChange::Opacity(value))
        })
        .step(0.01_f32)
        .width(160)
        .style(style::inspector_slider);
        col = col.push(
            row![
                text("Opacity"),
                opacity,
                text(format!("{:.0}%", glass.opacity * 100.0)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    col.into()
}

/// Alert delivery toggles and the list of alert conditions the loaded
/// indicators declare.
/// The catalog.json address the indicator browser loads.
fn catalog_url_view(url: &str) -> Element<'_, Message> {
    let input = iced::widget::text_input(
        data::chart::kline::indicators::manifest::DEFAULT_CATALOG_URL,
        url,
    )
    .on_input(Message::CatalogUrlChanged)
    .size(crate::style::text_size::BODY);
    column![
        input,
        text("HTTPS address of a catalog.json made with orbit-tape-indicator catalog. Press Refresh in the browser after changing it.")
            .size(crate::style::text_size::SMALL),
    ]
    .spacing(4)
    .into()
}

/// ntfy push messages (phone): topic, server, test.
fn ntfy_view<'a>(
    alerts: &'a Alerts,
    ntfy: &'a NtfySettings,
    saved: &'a NtfySettings,
) -> Element<'a, Message> {
    let topic = iced::widget::text_input("Topic, e.g. orbit-tape-7f3k9q2m", &ntfy.topic)
        .on_input(|topic| Message::Alert(AlertChange::NtfyTopic(topic)))
        .on_submit(Message::Alert(AlertChange::SaveNtfy))
        .size(crate::style::text_size::BODY);
    let server = iced::widget::text_input(
        &format!("Server (empty = {DEFAULT_NTFY_SERVER})"),
        &ntfy.server,
    )
    .on_input(|server| Message::Alert(AlertChange::NtfyServer(server)))
    .on_submit(Message::Alert(AlertChange::SaveNtfy))
    .size(crate::style::text_size::BODY);
    let dirty = ntfy.topic.trim() != saved.topic || ntfy.server.trim() != saved.server;
    let buttons = row![
        button(text("Save")).on_press_maybe(dirty.then_some(Message::Alert(AlertChange::SaveNtfy))),
        button(text("Send test")).on_press_maybe(
            (saved.is_set() && !dirty).then_some(Message::Alert(AlertChange::TestNtfy))
        ),
    ]
    .spacing(8);
    let chart_alerts = tooltip(
        checkbox(alerts.ntfy)
            .label("Chart alerts to ntfy too")
            .on_toggle(|on| Message::Alert(AlertChange::ChartNtfy(on))),
        Some(
            "Indicator alerts of the open charts. Saved alerts (the alert list) each pick desktop, ntfy or both.",
        ),
        Position::Top,
    );
    column![
        text("Phone (ntfy)")
            .size(crate::style::text_size::SMALL)
            .style(style::secondary_text),
        topic,
        server,
        buttons,
        row![chart_alerts].height(24).align_y(Alignment::Center),
        text("Subscribe to the topic in the ntfy app. Anyone who knows the topic can read the alerts, so pick one that is hard to guess.")
            .size(crate::style::text_size::SMALL),
    ]
    .spacing(6)
    .into()
}

fn alerts_view(alerts: &Alerts) -> Element<'_, Message> {
    let enabled = tooltip(
        checkbox(alerts.enabled)
            .label("Indicator alerts")
            .on_toggle(|on| Message::Alert(AlertChange::Enabled(on))),
        Some(
            "Alert when an indicator condition is true on a newly closed bar.\n- Only while the app runs, on charts showing the indicator.",
        ),
        Position::Top,
    );
    let mut col = column![row![enabled].height(24).align_y(Alignment::Center)].spacing(4);
    if !alerts.enabled {
        return col.into();
    }

    let notification = checkbox(alerts.notification)
        .label("Desktop notification")
        .on_toggle(|on| Message::Alert(AlertChange::Notification(on)));
    let sound = checkbox(alerts.sound)
        .label("Sound")
        .on_toggle(|on| Message::Alert(AlertChange::Sound(on)));
    col = col
        .push(row![notification].height(24).align_y(Alignment::Center))
        .push(row![sound].height(24).align_y(Alignment::Center));
    if alerts.sound {
        let volume = slider(0.0..=100.0, alerts.volume, |value| {
            Message::Alert(AlertChange::Volume(value))
        })
        .step(1.0_f32)
        .width(120)
        .style(style::inspector_slider);
        col = col.push(
            row![
                text("Volume"),
                volume,
                text(format!("{:.0}%", alerts.volume))
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    col = col.push(button(text("Test alert")).on_press(Message::Alert(AlertChange::Test)));

    let mut conditions = column![].spacing(4);
    let mut any = false;
    for registration in data::chart::kline::indicators::catalog().registrations() {
        let definition = &registration.definition;
        let outputs = definition
            .outputs
            .iter()
            .filter(|output| output.alert)
            .collect::<Vec<_>>();
        if outputs.is_empty() {
            continue;
        }
        any = true;
        conditions = conditions.push(
            text(definition.display_name.clone())
                .size(crate::style::text_size::SMALL)
                .style(style::secondary_text),
        );
        for output in outputs {
            let key = data::chart::kline::alerts::condition_key(&registration.id, &output.id);
            let on = !alerts.is_muted(&key);
            conditions = conditions.push(
                checkbox(on)
                    .label(output.label.clone())
                    .on_toggle(move |on| Message::Alert(AlertChange::Condition(key.clone(), on))),
            );
        }
    }
    if !any {
        conditions = conditions.push(
            text("No loaded indicator declares alerts")
                .size(crate::style::text_size::SMALL)
                .style(style::secondary_text),
        );
    }
    col.push(conditions).into()
}
