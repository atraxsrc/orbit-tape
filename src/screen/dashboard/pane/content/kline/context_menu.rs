//! The chart's right-click menu: add a price alert at the clicked price, arm
//! and remove the alert on the line under the cursor, and clear the chart.

use crate::style;
use crate::widget::chart::kline::drawing::{DrawingAnchor, DrawingId};
use iced::widget::{button, column, container, mouse_area, opaque, responsive, space, text};
use iced::{Element, Length, Padding, Point};

/// Rough menu size, used to keep it inside the chart near the edges.
const MENU_WIDTH: f32 = 200.0;
const ITEM_HEIGHT: f32 = 26.0;

/// The alert state of the line under the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineAlert {
    /// A price-panel horizontal line without an alert.
    None,
    Armed,
    Fired,
}

#[derive(Debug, Clone)]
pub(super) struct ContextMenu {
    position: Point,
    /// Point under the cursor and its formatted price, in the price panel.
    add_at: Option<(DrawingAnchor, String)>,
    /// Line under the cursor that can carry an alert.
    line: Option<(DrawingId, LineAlert)>,
    counts: ChartCounts,
}

/// How many things the "Remove all" entries would remove.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ChartCounts {
    pub alerts: usize,
    pub drawings: usize,
    pub indicators: usize,
    pub studies: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum Message {
    Close,
    AddAlert(DrawingAnchor),
    ArmAlert(DrawingId),
    RemoveAlert(DrawingId),
    RemoveAllAlerts,
    RemoveAllDrawings,
    RemoveAllIndicators,
    RemoveAllStudies,
}

impl ContextMenu {
    /// A menu with at least one entry, or `None`.
    pub(super) fn new(
        position: Point,
        add_at: Option<(DrawingAnchor, String)>,
        line: Option<(DrawingId, LineAlert)>,
        counts: ChartCounts,
    ) -> Option<Self> {
        let menu = Self {
            position,
            add_at,
            line,
            counts,
        };
        (!menu.items().is_empty()).then_some(menu)
    }

    fn items(&self) -> Vec<(String, Message)> {
        let mut items = Vec::new();
        if let Some((id, alert)) = self.line {
            match alert {
                LineAlert::None => {
                    items.push(("Add alert to line".to_owned(), Message::ArmAlert(id)))
                }
                LineAlert::Armed => {
                    items.push(("Remove alert".to_owned(), Message::RemoveAlert(id)));
                }
                LineAlert::Fired => {
                    items.push(("Re-arm alert".to_owned(), Message::ArmAlert(id)));
                    items.push(("Remove alert".to_owned(), Message::RemoveAlert(id)));
                }
            }
        }
        if let Some((anchor, price)) = &self.add_at {
            items.push((format!("Add alert at {price}"), Message::AddAlert(*anchor)));
        }
        let counts = self.counts;
        for (count, what, message) in [
            (counts.alerts, "alerts", Message::RemoveAllAlerts),
            (counts.drawings, "drawings", Message::RemoveAllDrawings),
            (
                counts.indicators,
                "indicators",
                Message::RemoveAllIndicators,
            ),
            (counts.studies, "studies", Message::RemoveAllStudies),
        ] {
            if count > 0 {
                items.push((format!("Remove all {what} ({count})"), message));
            }
        }
        items
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let items = self.items();
        let item_count = items.len();
        let position = self.position;

        // Any click outside the menu closes it.
        let backdrop = mouse_area(
            container(space::horizontal())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::Close)
        .on_right_press(Message::Close)
        .on_middle_press(Message::Close);

        let menu = responsive(move |size| {
            let mut list = column![].spacing(2).width(Length::Fixed(MENU_WIDTH));
            for (label, message) in &items {
                list = list.push(
                    button(text(label.clone()).size(12))
                        .style(|theme, status| style::button::transparent(theme, status, false))
                        .width(Length::Fill)
                        .padding([4, 8])
                        .on_press(*message),
                );
            }
            let height = ITEM_HEIGHT * item_count as f32 + 8.0;
            let left = if position.x + MENU_WIDTH > size.width {
                (position.x - MENU_WIDTH).max(0.0)
            } else {
                position.x
            };
            let top = if position.y + height > size.height {
                (position.y - height).max(0.0)
            } else {
                position.y
            };
            container(opaque(
                container(list).style(style::chart_info_tooltip).padding(4),
            ))
            .padding(Padding {
                top,
                left,
                ..Padding::ZERO
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        });

        iced::widget::Stack::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(backdrop)
            .push(menu)
            .into()
    }
}
