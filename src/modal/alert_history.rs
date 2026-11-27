//! Sidebar alert list: the saved alerts (`alerts.json`, made in the app or
//! by Claude) and, below, the alerts that fired, newest first. Clicking a
//! fired alert focuses a chart pane showing that ticker.

use crate::style;
use data::config::alert_history::{AlertHistory, FiredAlert};
use data::config::alert_store::{AlertStore, Channel, Creator, Frequency, StoredAlert};
use iced::widget::{button, column, container, pick_list, row, scrollable, space, text};
use iced::{Alignment, Element, Length};

#[derive(Debug, Clone)]
pub enum Message {
    /// Focus the chart for the entry at this index (newest first).
    Focus(usize),
    Clear,
    /// Turn a saved alert on (resume) or off (pause).
    SetEnabled(String, bool),
    SetFrequency(String, Frequency),
    SetChannel(String, Channel),
    Delete(String),
}

/// Which program evaluates the saved alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    ThisApp,
    /// `orbit-tape watch` (or another copy of the app).
    Elsewhere,
}

pub fn view<'a>(
    history: &'a AlertHistory,
    store: &'a AlertStore,
    runner: Runner,
) -> Element<'a, Message> {
    let saved = saved_alerts(store, runner);
    let fired = fired_alerts(history);
    container(
        scrollable(
            column![saved, fired]
                .spacing(20)
                .padding(iced::padding::right(12)),
        )
        .height(Length::Shrink),
    )
    .width(380)
    .max_height(620)
    .padding(16)
    .style(style::dashboard_modal)
    .into()
}

fn saved_alerts(store: &AlertStore, runner: Runner) -> Element<'_, Message> {
    let header = text(format!("Saved alerts ({} active)", store.active_count()))
        .size(style::text_size::SECTION);
    let runner = text(match runner {
        Runner::ThisApp => {
            "Run by this app. Start orbit-tape watch to keep them running when the app is closed."
        }
        Runner::Elsewhere => "Run by orbit-tape watch, also while the app is closed.",
    })
    .size(style::text_size::SMALL)
    .style(style::secondary_text);
    let mut col = column![header, runner].spacing(6);
    if store.alerts.is_empty() {
        return col
            .push(
                text("None yet. Right-click a chart to add a price alert, or ask Claude.")
                    .size(style::text_size::BODY),
            )
            .into();
    }
    for alert in &store.alerts {
        col = col.push(saved_alert(alert));
    }
    col.into()
}

fn saved_alert(alert: &StoredAlert) -> Element<'_, Message> {
    let by = match alert.created_by {
        Creator::App => "",
        Creator::Claude => "Claude",
    };
    // When it last fired, in local time; every-time alerts keep firing.
    let fired = alert.fired_at.and_then(|ms| {
        chrono::DateTime::from_timestamp_millis(ms as i64).map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%d %b %H:%M")
                .to_string()
        })
    });
    let state = match (alert.is_done(), alert.enabled, fired) {
        (true, _, Some(time)) => format!("fired {time}"),
        (true, _, None) => "fired".to_owned(),
        (false, true, Some(time)) => format!("last fired {time}"),
        (false, true, None) => String::new(),
        (false, false, Some(time)) => format!("paused, last fired {time}"),
        (false, false, None) => "paused".to_owned(),
    };
    let title = row![
        text(format!(
            "{} {} {}",
            alert.symbol(),
            alert.exchange_name(),
            alert.timeframe
        ))
        .size(style::text_size::BODY),
        space::horizontal(),
        text(by)
            .size(style::text_size::SMALL)
            .style(|theme: &iced::Theme| text::Style {
                color: Some(theme.extended_palette().primary.base.color),
            }),
        text(state)
            .size(style::text_size::SMALL)
            .style(style::secondary_text),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let mut condition = alert.condition();
    if !alert.message.trim().is_empty() {
        condition = format!("{condition}\n{}", alert.message.trim());
    }
    let id = alert.id.clone();
    let toggle_id = id.clone();
    let frequency_id = id.clone();
    let channel_id = id.clone();
    let controls = row![
        button(text(if alert.enabled { "Pause" } else { "Resume" }).size(style::text_size::SMALL))
            .on_press(Message::SetEnabled(toggle_id, !alert.enabled))
            .style(|theme, status| style::button::transparent(theme, status, false)),
        pick_list(Frequency::ALL, Some(alert.frequency), move |frequency| {
            Message::SetFrequency(frequency_id.clone(), frequency)
        })
        .text_size(style::text_size::SMALL),
        pick_list(Channel::ALL, Some(alert.channel), move |channel| {
            Message::SetChannel(channel_id.clone(), channel)
        })
        .text_size(style::text_size::SMALL),
        space::horizontal(),
        button(text("Delete").size(style::text_size::SMALL))
            .on_press(Message::Delete(id))
            .style(|theme, status| style::button::transparent(theme, status, false)),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let mut col = column![
        title,
        text(condition).size(style::text_size::SMALL),
        controls
    ]
    .spacing(4);
    if let Some(error) = &alert.error {
        col = col.push(text(error.as_str()).size(style::text_size::SMALL).style(
            |theme: &iced::Theme| text::Style {
                color: Some(theme.extended_palette().danger.base.color),
            },
        ));
    }
    container(col).padding([6, 6]).into()
}

fn fired_alerts(history: &AlertHistory) -> Element<'_, Message> {
    let mut header = row![
        text(format!("Fired ({})", history.len())).size(style::text_size::SECTION),
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
        column(rows).spacing(2).into()
    };

    column![header, body].spacing(12).into()
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
