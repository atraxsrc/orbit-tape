use iced::{
    Alignment, Element,
    widget::{button, column, container, pick_list, row, space, text, text_input::default},
};

use crate::{
    style::{self, Icon, icon_text},
    theme::ThemeState,
    widget::{color_picker::color_picker, color_swatch_button},
};
use data::config::theme::ACCENT_COUNT;
use iced_core::Color;
use palette::Hsva;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Component {
    Background,
    Text,
    Primary,
    Success,
    Danger,
    Warning,
    /// One of the accent colors used by indicators and drawings, by index.
    Accent(usize),
}

impl std::fmt::Display for Component {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accent(index) => write!(f, "Accent {}", index + 1),
            _ => write!(f, "{self:?}"),
        }
    }
}

impl Component {
    fn all() -> Vec<Self> {
        let mut all = vec![
            Self::Background,
            Self::Text,
            Self::Primary,
            Self::Success,
            Self::Danger,
            Self::Warning,
        ];
        all.extend((0..ACCENT_COUNT).map(Self::Accent));
        all
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    ComponentChanged(Component),
    CloseRequested,
    Color(Hsva),
    HexInput(String),
    /// Gives the focused accent back to the palette.
    ResetAccent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    UpdateTheme(iced_core::Theme),
    /// Sets an accent color by hand, or with `None` gives it back to the
    /// palette.
    SetAccent(usize, Option<Color>),
    Exit,
}

pub struct ThemeEditor {
    pub custom_theme: Option<iced_core::Theme>,
    component: Component,
    hex_input: Option<String>,
    editing: Option<Hsva>,
}

impl ThemeEditor {
    pub fn new(custom_theme: Option<data::Theme>) -> Self {
        Self {
            custom_theme: custom_theme.map(|theme| theme.0),
            component: Component::Background,
            hex_input: None,
            editing: None,
        }
    }

    pub fn reset_input(&mut self) {
        self.hex_input = None;
        self.editing = None;
    }

    /// Focuses one accent color, for opening the editor on it.
    pub fn focus_accent(&mut self, index: usize) {
        self.component = Component::Accent(index.min(ACCENT_COUNT - 1));
        self.reset_input();
    }

    fn focused_color(&self, theme: &ThemeState) -> Color {
        let palette = theme.current().palette();
        match self.component {
            Component::Background => palette.background,
            Component::Text => palette.text,
            Component::Primary => palette.primary,
            Component::Success => palette.success,
            Component::Danger => palette.danger,
            Component::Warning => palette.warning,
            Component::Accent(index) => theme.accents().as_array()[index],
        }
    }

    /// Applies `color` to the focused component.
    fn apply(&mut self, color: Color, theme: &ThemeState) -> Action {
        let mut new_palette = theme.current().palette();
        match self.component {
            Component::Background => new_palette.background = color,
            Component::Text => new_palette.text = color,
            Component::Primary => new_palette.primary = color,
            Component::Success => new_palette.success = color,
            Component::Danger => new_palette.danger = color,
            Component::Warning => new_palette.warning = color,
            Component::Accent(index) => return Action::SetAccent(index, Some(color)),
        }

        let new_theme = iced_core::Theme::custom("Custom".to_string(), new_palette);
        self.custom_theme = Some(new_theme.clone());
        Action::UpdateTheme(new_theme)
    }

    pub fn update(&mut self, message: Message, theme: &ThemeState) -> Option<Action> {
        match message {
            Message::Color(hsva) => {
                self.hex_input = None;
                self.editing = Some(hsva);
                Some(self.apply(data::config::theme::from_hsva(hsva), theme))
            }
            Message::ComponentChanged(component) => {
                self.component = component;
                self.hex_input = None;
                let color = self.focused_color(theme);
                self.editing = Some(data::config::theme::to_hsva(color));
                None
            }
            Message::HexInput(input) => {
                let action = data::config::theme::hex_to_color(&input).map(|color| {
                    self.editing = Some(data::config::theme::to_hsva(color));
                    self.apply(color, theme)
                });
                self.hex_input = Some(input);
                action
            }
            Message::ResetAccent => {
                let Component::Accent(index) = self.component else {
                    return None;
                };
                self.reset_input();
                Some(Action::SetAccent(index, None))
            }
            Message::CloseRequested => Some(Action::Exit),
        }
    }

    pub fn view(&self, theme: &ThemeState) -> Element<'_, Message> {
        let color = self.focused_color(theme);
        let hsva_in = self
            .editing
            .unwrap_or_else(|| data::config::theme::to_hsva(color));

        let close_editor = button(icon_text(Icon::Return, 11)).on_press(Message::CloseRequested);

        let is_input_valid = self.hex_input.is_none()
            || self
                .hex_input
                .as_deref()
                .and_then(data::config::theme::hex_to_color)
                .is_some();

        let hex_input = iced::widget::text_input(
            "",
            self.hex_input
                .as_deref()
                .unwrap_or(data::config::theme::color_to_hex(color).as_str()),
        )
        .on_input(Message::HexInput)
        .width(80)
        .style(move |theme: &iced::Theme, status| {
            let palette = theme.extended_palette();

            iced::widget::text_input::Style {
                border: iced::Border {
                    color: if is_input_valid {
                        palette.background.strong.color
                    } else {
                        palette.danger.base.color
                    },
                    width: 1.0,
                    radius: crate::style::radius::of(2.0),
                },
                ..default(theme, status)
            }
        });

        let focused_field = pick_list(
            Component::all(),
            Some(self.component),
            Message::ComponentChanged,
        );

        let mut content = column![
            row![
                close_editor,
                space::horizontal(),
                row![hex_input, focused_field,].spacing(4),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            color_picker(hsva_in, Message::Color),
            accent_row(theme, self.component),
        ]
        .spacing(10);

        if let Component::Accent(index) = self.component
            && theme.accent_overrides()[index].is_some()
        {
            content = content.push(
                row![
                    text("Set by hand")
                        .size(style::text_size::SMALL)
                        .style(style::secondary_text),
                    space::horizontal(),
                    button(text("Use palette color")).on_press(Message::ResetAccent),
                ]
                .align_y(Alignment::Center),
            );
        }

        container(content)
            .max_width(380)
            .padding(24)
            .style(style::dashboard_modal)
            .into()
    }
}

/// The accent colors as squares; clicking one focuses it for editing.
fn accent_row(theme: &ThemeState, focused: Component) -> Element<'static, Message> {
    let squares = accent_squares(theme, focused, |index| {
        Message::ComponentChanged(Component::Accent(index))
    });
    column![
        text("Accent colors (indicators and drawings)").size(style::text_size::SMALL),
        squares,
    ]
    .spacing(4)
    .into()
}

/// The accent colors in use as a row of squares, the focused one highlighted.
pub fn accent_squares<Message>(
    theme: &ThemeState,
    focused: Component,
    on_press: impl Fn(usize) -> Message,
) -> Element<'static, Message>
where
    Message: Clone + 'static,
{
    let colors = *theme.accents().as_array();
    row(colors.into_iter().enumerate().map(|(index, color)| {
        color_swatch_button(
            move |_: &iced::Theme| color,
            focused == Component::Accent(index),
            on_press(index),
        )
    }))
    .spacing(2)
    .into()
}
