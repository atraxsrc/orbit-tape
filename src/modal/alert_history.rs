//! Sidebar list of fired alerts, newest first. Clicking an entry focuses a
//! chart pane showing that ticker.

use crate::style;
use data::config::alert_history::{AlertHistory, FiredAlert};
use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length};

#[derive(Debug, Clone)]
pub enum Message {
    /// Focus the chart for the entry at this index (newest first).
    Focus(usize),
    Clear,
}

pub fn view(history: &AlertHistory) -> Element<'_, Message> {
    let mut header = row![
        text(format!("Alerts ({})", history.len())).size(style::text_size::SECTION),
        space::horizontal(),
    ]
    .align_y(Alignment::Center);
    if !history.is_empty() {
        header = header.push(
            button(text("Clear").size(style::text_size::BODY))
                .on_press(Message::Clear)
                .style(|theme, status| style::button::transparent(theme, status, false)),
        );
    }

    let body: Element<'_, Message> = if history.is_empty() {
        text("No alerts have fired yet.")
            .size(style::text_size::BODY)
            .into()
    } else {
        let rows = history
            .entries()
            .enumerate()
            .map(|(index, alert)| entry(index, alert, history.is_unread(index)));
        scrollable(column(rows).spacing(2).padding(iced::padding::right(12)))
            .height(Length::Shrink)
            .into()
    };

    container(column![header, body].spacing(12))
        .width(360)
        .max_height(560)
        .padding(16)
        .style(style::dashboard_modal)
        .into()
}

fn entry(index: usize, alert: &FiredAlert, unread: bool) -> Element<'_, Message> {
    let time = chrono::DateTime::from_timestamp_millis(alert.fired_at as i64)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%d %b %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();

    let dot = text(if unread { "●" } else { " " })
        .size(style::text_size::SMALL)
        .style(|theme: &iced::Theme| text::Style {
            color: Some(theme.extended_palette().primary.base.color),
        });

    let top = row![
        dot,
        text(format!("{} {}", alert.symbol, alert.basis)).size(style::text_size::BODY),
        space::horizontal(),
        text(time).size(style::text_size::SMALL),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let bottom = row![
        text(if alert.missed {
            format!(
                "{}: {} ({})",
                alert.source,
                alert.condition,
                crate::alerts::MISSED_LABEL
            )
        } else {
            format!("{}: {}", alert.source, alert.condition)
        })
        .size(style::text_size::SMALL)
        .width(Length::Fill),
        text(&alert.price).size(style::text_size::SMALL),
    ]
    .spacing(6)
    .padding(iced::padding::left(14));

    button(column![top, bottom].spacing(2))
        .width(Length::Fill)
        .padding([4, 6])
        .on_press(Message::Focus(index))
        .style(|theme, status| style::button::menu_body(theme, status, false))
        .into()
}
