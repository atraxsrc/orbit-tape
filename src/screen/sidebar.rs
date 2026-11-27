use self::tickers_table::TickersTable;
use crate::{
    TooltipPosition,
    style::{Icon, icon_text},
    widget::button_with_tooltip,
};
use data::sidebar;

use iced::{
    Alignment, Element, Subscription, Task,
    widget::responsive,
    widget::{column, row, space},
};
use rustc_hash::FxHashMap;

pub mod tickers_table;
pub mod watchlist;

#[derive(Debug, Clone)]
pub enum Message {
    ToggleSidebarMenu(Option<sidebar::Menu>),
    SetSidebarPosition(sidebar::Position),
    TickersTable(self::tickers_table::Message),
    ToggleWatchlist,
    Watchlist(self::watchlist::Message),
}

pub struct Sidebar {
    pub state: data::Sidebar,
    pub tickers_table: TickersTable,
    /// The watchlist shows its edit controls.
    watchlist_editing: bool,
}

pub enum Action {
    TickerSelected(
        exchange::TickerInfo,
        Option<data::layout::pane::ContentKind>,
    ),
    ErrorOccurred(data::InternalError),
    MenuChanged(Option<sidebar::Menu>),
    /// A ticker clicked in the watchlist, for the panes of this link group.
    WatchlistSelected(exchange::TickerInfo, Option<data::layout::pane::LinkGroup>),
    /// The watchlist or the favorites changed: save the state now, so the
    /// change survives the app being killed instead of closed.
    Persist,
}

impl Sidebar {
    pub fn new(
        sidebar_state: &data::Sidebar,
        cache_market_metadata: bool,
        handles: exchange::adapter::AdapterHandles,
    ) -> (Self, Task<Message>) {
        let metadata = data::MarketMetadata::with_cache_enabled(cache_market_metadata);

        let (tickers_table, initial_fetch) =
            if let Some(settings) = sidebar_state.tickers_table.as_ref() {
                TickersTable::new_with_settings(settings, handles.clone(), metadata)
            } else {
                TickersTable::new(handles, metadata)
            };

        let mut state = sidebar_state.clone();
        state.watchlist.sync(&tickers_table.favorited_tickers);

        (
            Self {
                state,
                tickers_table,
                watchlist_editing: false,
            },
            initial_fetch.map(Message::TickersTable),
        )
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Action>) {
        match message {
            Message::ToggleSidebarMenu(menu) => {
                let new_menu = menu.filter(|&m| !self.is_menu_active(m));
                self.set_menu(new_menu);
                return (Task::none(), Some(Action::MenuChanged(new_menu)));
            }
            Message::SetSidebarPosition(position) => {
                self.state.position = position;
            }
            Message::ToggleWatchlist => {
                let watchlist = &mut self.state.watchlist;
                watchlist.open = !watchlist.open;
                self.watchlist_editing = false;
                if watchlist.open {
                    // Fresh prices now rather than at the next 5 minute tick.
                    return self.update(Message::TickersTable(tickers_table::Message::FetchStats));
                }
            }
            Message::Watchlist(msg) => {
                let saves_now = msg.saves_now();
                let leaves_edit_mode =
                    matches!(msg, watchlist::Message::ToggleEdit) && self.watchlist_editing;
                let before = self.state.watchlist.clone();
                let (task, action) = self.update_watchlist(msg);
                let changed = saves_now && self.state.watchlist != before;
                if action.is_none() && (changed || leaves_edit_mode) {
                    return (task, Some(Action::Persist));
                }
                return (task, action);
            }
            Message::TickersTable(msg) => {
                let action = self.tickers_table.update(msg);
                // Starring or unstarring in the table changes the watchlist.
                let changed = self
                    .state
                    .watchlist
                    .sync(&self.tickers_table.favorited_tickers);

                match action {
                    Some(tickers_table::Action::TickerSelected(ticker_info, content)) => {
                        return (
                            Task::none(),
                            Some(Action::TickerSelected(ticker_info, content)),
                        );
                    }
                    Some(tickers_table::Action::Fetch(task)) => {
                        return (task.map(Message::TickersTable), None);
                    }
                    Some(tickers_table::Action::ErrorOccurred(error)) => {
                        return (Task::none(), Some(Action::ErrorOccurred(error)));
                    }
                    Some(tickers_table::Action::FocusWidget(id)) => {
                        return (iced::widget::operation::focus(id), None);
                    }
                    None if changed => return (Task::none(), Some(Action::Persist)),
                    None => {}
                }
            }
        }

        (Task::none(), None)
    }

    fn update_watchlist(&mut self, message: watchlist::Message) -> (Task<Message>, Option<Action>) {
        let list = &mut self.state.watchlist;
        match message {
            watchlist::Message::Select(ticker) => {
                if let Some(info) = self.tickers_table.ticker_info(&ticker) {
                    return (
                        Task::none(),
                        Some(Action::WatchlistSelected(info, list.link_group)),
                    );
                }
            }
            watchlist::Message::ToggleSection(index) => {
                list.toggle_collapsed(index);
            }
            watchlist::Message::ToggleEdit => {
                self.watchlist_editing = !self.watchlist_editing;
            }
            watchlist::Message::MoveTicker(ticker, direction) => {
                list.move_ticker(ticker, direction);
            }
            watchlist::Message::MoveSection(index, direction) => {
                list.move_section(index, direction);
            }
            watchlist::Message::RenameSection(index, name) => {
                list.rename_section(index, name);
            }
            watchlist::Message::AddSection => list.add_section(),
            watchlist::Message::SetLinkGroup(group) => list.link_group = group,
            watchlist::Message::SortBy(column) => list.cycle_sort(column),
            watchlist::Message::DragSection(crate::widget::column_drag::DragEvent::Dropped {
                index,
                target_index,
            }) => {
                list.drop_section(index, target_index);
            }
            watchlist::Message::DragSection(_) => {}
            watchlist::Message::SetSectionColor(index, color) => {
                list.set_section_color(index, color);
            }
            watchlist::Message::ResizeColumn(index, dx) => {
                list.resize_column(index, dx);
            }
            watchlist::Message::Resize(dx) => {
                // The handle is on the panel's outer edge.
                let dx = match self.state.position {
                    sidebar::Position::Left => dx,
                    sidebar::Position::Right => -dx,
                };
                list.resize(dx);
            }
            watchlist::Message::RemoveSection(index) => {
                list.remove_section(index);
            }
            watchlist::Message::Unstar(ticker) => {
                if self.tickers_table.favorited_tickers.contains(&ticker) {
                    return self.update(Message::TickersTable(
                        tickers_table::Message::FavoriteTicker(ticker),
                    ));
                }
            }
        }
        (Task::none(), None)
    }

    pub fn view(
        &self,
        audio_volume: Option<f32>,
        unread_alerts: usize,
        accents: crate::theme::ThemeAccents,
    ) -> Element<'_, Message> {
        let state = &self.state;

        let tooltip_position = if state.position == sidebar::Position::Left {
            TooltipPosition::Right
        } else {
            TooltipPosition::Left
        };

        let is_table_open = self.tickers_table.is_shown;

        let nav_buttons =
            self.nav_buttons(is_table_open, audio_volume, unread_alerts, tooltip_position);

        let tickers_table = if is_table_open {
            column![responsive(move |size| self
                .tickers_table
                .view(size)
                .map(Message::TickersTable))]
            .width(200)
        } else {
            column![]
        };

        let watchlist: Element<'_, Message> = if state.watchlist.open {
            let panel = watchlist::view(
                &state.watchlist,
                &self.tickers_table,
                self.watchlist_editing,
                accents,
            )
            .map(Message::Watchlist);
            let handle = crate::widget::resize_handle::resize_handle(|dx| {
                Message::Watchlist(watchlist::Message::Resize(dx))
            });
            // The handle sits on the edge away from the nav bar.
            match state.position {
                sidebar::Position::Left => row![panel, handle].spacing(2).into(),
                sidebar::Position::Right => row![handle, panel].spacing(2).into(),
            }
        } else {
            column![].into()
        };
        let any_open = is_table_open || state.watchlist.open;

        match state.position {
            sidebar::Position::Left => row![nav_buttons, tickers_table, watchlist],
            sidebar::Position::Right => row![watchlist, tickers_table, nav_buttons],
        }
        .spacing(if any_open { 8 } else { 4 })
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        self.tickers_table
            .subscription(self.state.watchlist.open)
            .map(Message::TickersTable)
    }

    fn nav_buttons(
        &self,
        is_table_open: bool,
        audio_volume: Option<f32>,
        unread_alerts: usize,
        tooltip_position: TooltipPosition,
    ) -> iced::widget::Column<'_, Message> {
        let settings_modal_button = {
            let is_active = self.is_menu_active(sidebar::Menu::Settings)
                || self.is_menu_active(sidebar::Menu::ThemeEditor)
                || self.is_menu_active(sidebar::Menu::Network);

            button_with_tooltip(
                icon_text(Icon::Cog, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Settings)),
                None,
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            )
        };

        let layout_modal_button = {
            let is_active = self.is_menu_active(sidebar::Menu::Layout);

            button_with_tooltip(
                icon_text(Icon::Layout, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Layout)),
                None,
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            )
        };

        let ticker_search_button = {
            button_with_tooltip(
                icon_text(Icon::Search, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::TickersTable(self::tickers_table::Message::ToggleTable),
                None,
                tooltip_position,
                move |theme, status| {
                    crate::style::button::transparent(theme, status, is_table_open)
                },
            )
        };

        let audio_btn = {
            let is_active = self.is_menu_active(sidebar::Menu::Audio);

            let icon = match audio_volume.unwrap_or(0.0) {
                v if v >= 40.0 => Icon::SpeakerHigh,
                v if v > 0.0 => Icon::SpeakerLow,
                _ => Icon::SpeakerOff,
            };

            button_with_tooltip(
                icon_text(icon, 14).width(24).align_x(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Audio)),
                None,
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            )
        };

        let alerts_btn = {
            let is_active = self.is_menu_active(sidebar::Menu::Alerts);
            button_with_tooltip(
                crate::widget::bell::bell(unread_alerts),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Alerts)),
                None,
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            )
        };

        let watchlist_button = {
            let is_open = self.state.watchlist.open;
            button_with_tooltip(
                icon_text(Icon::StarFilled, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::ToggleWatchlist,
                Some("Watchlist"),
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_open),
            )
        };

        column![
            ticker_search_button,
            watchlist_button,
            layout_modal_button,
            audio_btn,
            alerts_btn,
            space::vertical(),
            settings_modal_button,
        ]
        .width(32)
        .spacing(8)
    }

    pub fn hide_tickers_table(&mut self) -> bool {
        let table = &mut self.tickers_table;

        if table.expand_ticker_card.is_some() {
            table.expand_ticker_card = None;
            return true;
        } else if table.is_shown {
            table.is_shown = false;
            return true;
        }

        false
    }

    pub fn is_menu_active(&self, menu: sidebar::Menu) -> bool {
        self.state.active_menu == Some(menu)
    }

    pub fn active_menu(&self) -> Option<sidebar::Menu> {
        self.state.active_menu
    }

    pub fn position(&self) -> sidebar::Position {
        self.state.position
    }

    pub fn set_menu(&mut self, menu: Option<sidebar::Menu>) {
        self.state.active_menu = menu;
    }

    pub fn sync_tickers_table_settings(&mut self) {
        let settings = &self.tickers_table.settings();
        self.state.tickers_table = Some(settings.clone());
    }

    pub fn tickers_info(&self) -> &FxHashMap<exchange::Ticker, Option<exchange::TickerInfo>> {
        self.tickers_table.metadata.tickers()
    }

    pub fn cache_enabled(&self) -> bool {
        self.tickers_table.metadata.cache_enabled()
    }

    pub fn set_cache_enabled(&mut self, enabled: bool) {
        self.tickers_table.metadata.set_cache_enabled(enabled);
    }

    pub fn force_refresh_metadata(&mut self) -> Option<Task<Message>> {
        self.tickers_table
            .force_refresh_metadata()
            .map(|task| task.map(Message::TickersTable))
    }

    pub fn persist_metadata_cache(&self) {
        if self.tickers_table.metadata.cache_enabled() {
            self.tickers_table.metadata.save_to_file();
        }
    }

    pub fn last_metadata_update(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.tickers_table.last_metadata_update()
    }

    pub fn is_metadata_loading(&self) -> bool {
        self.tickers_table.is_metadata_loading()
    }
}
