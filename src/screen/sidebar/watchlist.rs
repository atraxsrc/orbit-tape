//! The watchlist panel: the favorited tickers in sections, with last price
//! and the change since 00:00 UTC, like a TradingView watchlist. Clicking a
//! row loads the ticker into the panes of the watchlist's link group (the
//! number on each pane, 1 by default). Edit mode moves rows and
//! sections, renames, adds and removes sections, and unstars tickers.

use super::tickers_table::TickersTable;
use crate::style::{self, Icon, icon_text};
use crate::theme::ThemeAccents;
use crate::widget::{color_swatch_button, column_drag, resolve_theme_color, tooltip};
use data::config::theme::{ThemeAccent, ThemeColor, ThemeColorRole, ThemeColorShade};
use data::layout::pane::LinkGroup;
use data::watchlist::{
    Direction, Sort, SortColumn, SortKeys, SortOrder, Watchlist, compact_zeros, short_label,
    sort_rows,
};
use exchange::{Ticker, adapter::MarketKind};
use iced::{
    Alignment, Element, Length, Theme,
    alignment::Horizontal,
    padding,
    widget::{
        button, column, container, pick_list, row, scrollable, space, text, text_input,
        tooltip::Position as TooltipPosition,
    },
};

const ROW_HEIGHT: f32 = 26.0;
/// Height of a section title, the part a section is dragged by.
const SECTION_HEADER_HEIGHT: f32 = 26.0;
/// Space before each number column; in the header it holds the border
/// handle that resizes the column.
const COLUMN_GAP: f32 = 6.0;

/// Shown on a change that is 24h rolling because the exchange gives no
/// 00:00 UTC open (all but OKX).
const ROLLING_HINT: &str = "24h change: this exchange gives no 00:00 UTC open";

impl Message {
    /// Whether a change from this message is saved to disk right away.
    /// Continuous drags and typing are saved when they end (leaving edit
    /// mode) or on exit.
    pub fn saves_now(&self) -> bool {
        !matches!(
            self,
            Self::Resize(_)
                | Self::ResizeColumn(..)
                | Self::RenameSection(..)
                | Self::Select(_)
                | Self::DragSection(_)
        ) || matches!(
            self,
            Self::DragSection(column_drag::DragEvent::Dropped { .. })
        )
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Select(Ticker),
    ToggleSection(usize),
    ToggleEdit,
    MoveTicker(Ticker, Direction),
    MoveSection(usize, Direction),
    RenameSection(usize, String),
    AddSection,
    RemoveSection(usize),
    Unstar(Ticker),
    SetLinkGroup(Option<LinkGroup>),
    /// A column header click: ascending, descending, then the user's order.
    SortBy(SortColumn),
    /// Drag on the panel edge, pixels to the right.
    Resize(f32),
    /// A section dragged by its title.
    DragSection(column_drag::DragEvent),
    SetSectionColor(usize, Option<ThemeColor>),
    /// Drag on the header border left of number column 0 (Last), 1 (Chg)
    /// or 2 (Chg%), pixels to the right.
    ResizeColumn(usize, f32),
}

/// The link group choices: none (the focused pane) or 1 to 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroupChoice(Option<LinkGroup>);

impl std::fmt::Display for GroupChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(group) => write!(f, "{group}"),
            None => write!(f, "-"),
        }
    }
}

const GROUP_CHOICES: [GroupChoice; 10] = [
    GroupChoice(None),
    GroupChoice(Some(LinkGroup::A)),
    GroupChoice(Some(LinkGroup::B)),
    GroupChoice(Some(LinkGroup::C)),
    GroupChoice(Some(LinkGroup::D)),
    GroupChoice(Some(LinkGroup::E)),
    GroupChoice(Some(LinkGroup::F)),
    GroupChoice(Some(LinkGroup::G)),
    GroupChoice(Some(LinkGroup::H)),
    GroupChoice(Some(LinkGroup::I)),
];

/// The change column values of one row.
struct Change {
    abs: Option<String>,
    pct: String,
    up: Option<bool>,
    /// 24h rolling, not since 00:00 UTC.
    rolling: bool,
}

fn change(table: &TickersTable, ticker: &Ticker) -> Option<Change> {
    let stats = table.stats(ticker)?;
    let precision = table.ticker_info(ticker).map(|info| info.min_ticksize);
    let direction = |pct: f32| (pct != 0.0).then_some(pct > 0.0);

    Some(match stats.utc_day_change() {
        Some((abs, pct)) => Change {
            abs: precision.map(|precision| compact_zeros(&signed(abs.to_string(precision)))),
            pct: data::util::pct_change(pct),
            up: direction(pct),
            rolling: false,
        },
        None => Change {
            abs: None,
            pct: data::util::pct_change(stats.daily_price_chg),
            up: direction(stats.daily_price_chg),
            rolling: true,
        },
    })
}

/// "+12.5" for a rise; a fall already has its minus.
fn signed(value: String) -> String {
    if value.starts_with('-') || value.trim_start_matches(['0', '.']).is_empty() {
        value
    } else {
        format!("+{value}")
    }
}

fn change_color(theme: &Theme, up: Option<bool>) -> iced::widget::text::Style {
    let palette = theme.extended_palette();
    iced::widget::text::Style {
        color: Some(match up {
            Some(true) => palette.success.base.color,
            Some(false) => palette.danger.base.color,
            None => palette.background.base.text,
        }),
    }
}

fn icon_button<'a>(icon: Icon, message: Message) -> Element<'a, Message> {
    button(icon_text(icon, 11).align_x(Horizontal::Center))
        .on_press(message)
        .padding([2, 4])
        .style(|theme, status| style::button::transparent(theme, status, false))
        .into()
}

/// What a row sorts by, from the same numbers the row shows.
fn sort_keys(table: &TickersTable, ticker: &Ticker) -> SortKeys {
    let stats = table.stats(ticker);
    let (change, change_pct) = match stats.map(|stats| (stats, stats.utc_day_change())) {
        Some((_, Some((abs, pct)))) => (Some(abs.to_f64()), Some(f64::from(pct))),
        Some((stats, None)) => (None, Some(f64::from(stats.daily_price_chg))),
        None => (None, None),
    };
    SortKeys {
        symbol: short_label(
            &ticker.display_symbol_and_type().0,
            ticker.market_type() != MarketKind::Spot,
        )
        .0,
        last: stats.map(|stats| stats.mark_price.to_f64()),
        change,
        change_pct,
    }
}

/// A column header that sorts by its column; the active one shows an arrow.
fn header_cell<'a>(
    label: &'a str,
    column: SortColumn,
    sort: Option<Sort>,
    width: Length,
    align: Horizontal,
) -> Element<'a, Message> {
    let mut content = row![text(label).size(style::text_size::SMALL)]
        .spacing(3)
        .align_y(Alignment::Center);
    if let Some(sort) = sort.filter(|sort| sort.column == column) {
        let icon = match sort.order {
            SortOrder::Ascending => Icon::SortAsc,
            SortOrder::Descending => Icon::SortDesc,
        };
        content = content.push(icon_text(icon, 9));
    }
    let cell = container(content).width(Length::Fill).align_x(align);

    button(cell)
        .on_press(Message::SortBy(column))
        .width(width)
        .padding(0)
        .style(|theme, status| style::button::transparent(theme, status, false))
        .into()
}

/// The title colors offered in edit mode: the theme's roles, then its
/// accents. `None` is the normal text color.
fn title_colors() -> impl Iterator<Item = Option<ThemeColor>> {
    let role = |role| ThemeColor {
        role,
        shade: ThemeColorShade::Base,
        accent: None,
    };
    let roles = [
        ThemeColorRole::Primary,
        ThemeColorRole::Success,
        ThemeColorRole::Danger,
        ThemeColorRole::Warning,
    ]
    .map(role);
    let accents = ThemeAccent::ALL.map(|accent| ThemeColor {
        accent: Some(accent),
        ..role(ThemeColorRole::Primary)
    });
    std::iter::once(None).chain(roles.into_iter().chain(accents).map(Some))
}

fn title_color(theme: &Theme, color: Option<ThemeColor>, accents: ThemeAccents) -> iced::Color {
    let palette = theme.extended_palette();
    color.map_or(palette.background.base.text, |color| {
        resolve_theme_color(color, palette, accents.as_array())
    })
}

pub fn view<'a>(
    list: &'a Watchlist,
    table: &'a TickersTable,
    editing: bool,
    accents: ThemeAccents,
) -> Element<'a, Message> {
    let group_picker = tooltip(
        pick_list(
            GROUP_CHOICES,
            Some(GroupChoice(list.link_group)),
            |choice: GroupChoice| Message::SetLinkGroup(choice.0),
        )
        .text_size(style::text_size::BODY)
        .padding([2, 6]),
        Some("Clicked tickers open in the panes with this number (- = the focused pane)"),
        TooltipPosition::Bottom,
    );

    let title = row![
        text("Watchlist").size(style::text_size::SECTION),
        space::horizontal(),
        group_picker,
        tooltip(
            button(icon_text(Icon::Edit, 12).align_x(Horizontal::Center))
                .on_press(Message::ToggleEdit)
                .padding([4, 6])
                .style(move |theme, status| style::button::transparent(theme, status, editing)),
            Some(if editing { "Done" } else { "Edit the list" }),
            TooltipPosition::Bottom,
        ),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    let sort = list.sort;
    let right = Horizontal::Right;
    let [last_w, chg_w, pct_w] = list.columns;
    let border = crate::widget::resize_handle::resize_handle;
    let columns = row![
        header_cell(
            "Symbol",
            SortColumn::Symbol,
            sort,
            Length::Fill,
            Horizontal::Left
        ),
        border(|dx| Message::ResizeColumn(0, dx)),
        header_cell("Last", SortColumn::Last, sort, last_w.into(), right),
        border(|dx| Message::ResizeColumn(1, dx)),
        header_cell("Chg", SortColumn::Change, sort, chg_w.into(), right),
        border(|dx| Message::ResizeColumn(2, dx)),
        header_cell("Chg%", SortColumn::ChangePct, sort, pct_w.into(), right),
    ]
    .height(20)
    // Rows give up 6 px to the scrollbar beside them.
    .padding(padding::left(6).right(12))
    .align_y(Alignment::Center);

    let mut rows = column![].spacing(1);

    if list.is_empty() {
        rows = rows.push(
            text("Star tickers in the ticker list (search icon) to add them here.")
                .size(style::text_size::BODY),
        );
    }

    // Each section is one block: its title, then its rows. Outside edit
    // mode the blocks can be dragged by their title.
    let mut blocks: Vec<Element<'a, Message>> = Vec::with_capacity(list.sections.len());
    for (index, section) in list.sections.iter().enumerate() {
        let mut block = column![section_header(list, index, editing, accents)].spacing(1);
        if section.collapsed && !editing {
            blocks.push(block.into());
            continue;
        }
        // Edit mode shows the user's order, which the arrows change.
        match sort.filter(|_| !editing) {
            Some(sort) => {
                let mut sorted: Vec<(&Ticker, SortKeys)> = section
                    .tickers
                    .iter()
                    .map(|ticker| (ticker, sort_keys(table, ticker)))
                    .collect();
                sort_rows(&mut sorted, sort);
                for (ticker, _) in sorted {
                    block = block.push(ticker_row(table, ticker, editing, list.columns));
                }
            }
            None => {
                for ticker in &section.tickers {
                    block = block.push(ticker_row(table, ticker, editing, list.columns));
                }
            }
        }
        blocks.push(block.into());
    }

    if editing {
        rows = rows.extend(blocks);
    } else {
        rows = rows.push(
            column_drag::Column::with_children(blocks)
                .width(Length::Fill)
                .spacing(1)
                .grab_top(SECTION_HEADER_HEIGHT)
                .on_drag(Message::DragSection),
        );
    }

    if editing {
        rows = rows.push(
            button(text("+ Section").size(style::text_size::BODY))
                .on_press(Message::AddSection)
                .style(|theme, status| style::button::transparent(theme, status, false)),
        );
    }

    column![
        title,
        text("Chg is since 00:00 UTC").size(style::text_size::TINY),
        columns,
        scrollable(rows)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(3)
                    .scroller_width(3)
                    .spacing(3),
            ))
            .height(Length::Fill),
    ]
    .spacing(6)
    .padding(padding::right(8).top(4))
    .width(list.width)
    .into()
}

fn section_header(
    list: &Watchlist,
    index: usize,
    editing: bool,
    accents: ThemeAccents,
) -> Element<'_, Message> {
    let section = &list.sections[index];

    if editing {
        let mut swatches = row![].spacing(0).align_y(Alignment::Center);
        for color in title_colors() {
            swatches = swatches.push(color_swatch_button(
                move |theme: &Theme| title_color(theme, color, accents),
                section.color == color,
                Message::SetSectionColor(index, color),
            ));
        }
        let mut controls = row![
            text_input("Section name", &section.name)
                .on_input(move |name| Message::RenameSection(index, name))
                .size(style::text_size::BODY)
                .width(Length::Fill)
                // The picked color shows here too, not only outside edit mode.
                .style(
                    move |theme: &Theme, status| iced::widget::text_input::Style {
                        value: title_color(theme, section.color, accents),
                        ..iced::widget::text_input::default(theme, status)
                    }
                ),
            icon_button(Icon::ChevronUp, Message::MoveSection(index, Direction::Up)),
            icon_button(
                Icon::ChevronDown,
                Message::MoveSection(index, Direction::Down)
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center);
        if list.sections.len() > 1 {
            controls = controls.push(tooltip(
                icon_button(Icon::TrashBin, Message::RemoveSection(index)),
                Some("Remove the section; its tickers move to the one above"),
                TooltipPosition::Left,
            ));
        }
        return container(column![controls, swatches].spacing(2))
            .padding(padding::top(6))
            .into();
    }

    let chevron = if section.collapsed {
        Icon::ChevronRight
    } else {
        Icon::ChevronDown
    };
    button(
        row![
            icon_text(chevron, 10),
            text(&section.name)
                .size(style::text_size::BODY)
                .style(move |theme: &Theme| iced::widget::text::Style {
                    color: Some(title_color(theme, section.color, accents)),
                }),
            text(section.tickers.len().to_string()).size(style::text_size::TINY),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .on_press(Message::ToggleSection(index))
    .width(Length::Fill)
    .height(SECTION_HEADER_HEIGHT)
    .padding([4, 6])
    .style(|theme, status| style::button::transparent(theme, status, false))
    .into()
}

fn ticker_row<'a>(
    table: &'a TickersTable,
    ticker: &'a Ticker,
    editing: bool,
    [last_w, chg_w, pct_w]: [f32; 3],
) -> Element<'a, Message> {
    let (base, suffix) = short_label(
        &ticker.display_symbol_and_type().0,
        ticker.market_type() != MarketKind::Spot,
    );
    let symbol = row![
        icon_text(style::venue_icon(ticker.exchange.venue()), 11),
        text(base)
            .size(style::text_size::BODY)
            .wrapping(text::Wrapping::None),
        text(suffix)
            .size(style::text_size::TINY)
            .wrapping(text::Wrapping::None)
            .style(|theme: &Theme| iced::widget::text::Style {
                color: Some(
                    theme
                        .extended_palette()
                        .background
                        .weak
                        .text
                        .scale_alpha(0.6)
                ),
            }),
    ]
    .spacing(4)
    .clip(true)
    .align_y(Alignment::Center)
    .width(Length::Fill);

    if editing {
        return row![
            symbol,
            icon_button(Icon::ChevronUp, Message::MoveTicker(*ticker, Direction::Up)),
            icon_button(
                Icon::ChevronDown,
                Message::MoveTicker(*ticker, Direction::Down)
            ),
            tooltip(
                icon_button(Icon::Close, Message::Unstar(*ticker)),
                Some("Remove (unstar)"),
                TooltipPosition::Left,
            ),
        ]
        .spacing(2)
        .padding(padding::left(6))
        .height(ROW_HEIGHT)
        .align_y(Alignment::Center)
        .into();
    }

    let last = table
        .price_display(ticker)
        .map_or_else(|| "-".to_owned(), compact_zeros);
    let change = change(table, ticker);
    let up = change.as_ref().and_then(|change| change.up);
    let abs = change
        .as_ref()
        .and_then(|change| change.abs.clone())
        .unwrap_or_else(|| "-".to_owned());
    let pct = change
        .as_ref()
        .map_or_else(|| "-".to_owned(), |change| change.pct.clone());
    let rolling = change.as_ref().is_some_and(|change| change.rolling);

    let cell = |value: String, width: f32| {
        text(value)
            .size(style::text_size::BODY)
            .wrapping(text::Wrapping::None)
            .width(width)
            .align_x(Horizontal::Right)
            .style(move |theme: &Theme| change_color(theme, up))
    };

    let pct_cell: Element<'a, Message> = if rolling {
        tooltip(
            cell(format!("{pct}*"), pct_w),
            Some(ROLLING_HINT),
            TooltipPosition::Left,
        )
    } else {
        cell(pct, pct_w).into()
    };

    button(
        row![
            symbol,
            text(last)
                .size(style::text_size::BODY)
                .wrapping(text::Wrapping::None)
                .width(last_w)
                .align_x(Horizontal::Right),
            cell(abs, chg_w),
            pct_cell,
        ]
        // Lines up with the header's border handles.
        .spacing(COLUMN_GAP)
        .align_y(Alignment::Center),
    )
    .on_press(Message::Select(*ticker))
    .width(Length::Fill)
    .height(ROW_HEIGHT)
    .padding([0, 6])
    .style(|theme, status| style::button::transparent(theme, status, false))
    .into()
}

#[cfg(test)]
mod tests {
    use super::signed;

    #[test]
    fn rises_get_a_plus_and_zero_stays_bare() {
        assert_eq!(signed("12.5".into()), "+12.5");
        assert_eq!(signed("-3.1".into()), "-3.1");
        assert_eq!(signed("0.00".into()), "0.00");
        assert_eq!(signed("0".into()), "0");
        assert_eq!(signed("0.05".into()), "+0.05");
    }
}
