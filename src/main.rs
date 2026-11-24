/// Reverse-DNS app ID; also the Wayland app_id, the .desktop file name and the
/// icon name, so COSMIC matches windows to the launcher entry.
pub const APP_ID: &str = "io.github.atraxsrc.OrbitTape";

mod alert_channels;
mod alert_engine;
mod alerts;
mod audio;
mod blur;
mod connector;
mod daily_report;
mod deferred;
mod headless;
mod indicator_watch;
mod layout;
mod logger;
mod mcp;
mod modal;
mod notify;
mod scan;
mod screen;
mod style;
mod theme;
mod version;
mod watch;
mod widget;
mod window;

use connector::client::DataSources;
use data::chart::kline::indicators::manifest;
use data::config::cosmic::ThemeSource;
use data::{layout::WindowSpec, sidebar};
use deferred::Deferred;
use layout::{LayoutId, configuration};
use modal::dashboard_modal;
use modal::{
    LayoutManager, ThemeEditor,
    audio::AudioStream,
    network_editor::{self, NetworkEditor},
};
use notify::Notifications;
use screen::dashboard::{self, Dashboard};
use theme::ThemeState;
use widget::{
    toast::{self, Toast},
    tooltip,
};

use iced::{
    Alignment, Element, Subscription, Task, keyboard, padding,
    widget::{
        button, column, container, pane_grid, row, rule, text, tooltip::Position as TooltipPosition,
    },
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

fn main() {
    // Headless subcommands: no window, no logger, no data migration.
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.first().map(String::as_str) {
        Some("scan") => std::process::exit(scan::run(&arguments[1..])),
        Some("mcp") => std::process::exit(mcp::run(&arguments[1..])),
        Some("watch") => std::process::exit(watch::run(&arguments[1..])),
        Some("report") => std::process::exit(daily_report::run(&arguments[1..])),
        _ => {}
    }

    logger::install_panic_hook();

    // Before the logger, which creates the data directory.
    let migration = data::migration::migrate_legacy_data_dir();

    if let Err(err) = logger::setup(cfg!(debug_assertions)) {
        logger::report_stderr(&format!("Failed to initialize logger: {err}"));
    }

    match migration {
        data::migration::Outcome::NotNeeded => {}
        data::migration::Outcome::Copied { from, to } => {
            log::info!(
                "Copied Flowsurface data from {} to {} (the old folder is unchanged)",
                from.display(),
                to.display()
            );
        }
        data::migration::Outcome::Failed { from, to, error } => {
            log::error!(
                "Failed to copy Flowsurface data from {} to {}: {error}",
                from.display(),
                to.display()
            );
        }
    }

    if let Err(error) = data::initialize_indicator_catalog() {
        logger::report_stderr(&error);
        std::process::exit(1);
    }

    std::thread::spawn(data::cleanup_old_market_data);

    let boot = data::load_boot_settings();

    let daemon = iced::daemon(
        move || OrbitTape::new(boot),
        OrbitTape::update,
        OrbitTape::view,
    )
    .settings(iced::Settings {
        antialiasing: boot.antialiasing,
        fonts: vec![
            Cow::Borrowed(style::AZERET_MONO_BYTES),
            Cow::Borrowed(style::ICONS_BYTES),
        ],
        default_text_size: style::text_size::BODY.into(),
        ..Default::default()
    })
    .title(OrbitTape::title)
    .theme(OrbitTape::theme)
    .style(OrbitTape::style)
    .scale_factor(OrbitTape::scale_factor)
    .subscription(OrbitTape::subscription);

    if let Err(err) = daemon.run() {
        let message = format!("Runtime error: {err}");
        log::error!("{message}");
        logger::report_stderr(&message);
    }
}

const COSMIC_THEME_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

struct OrbitTape {
    main_window: window::Window,
    sidebar: screen::sidebar::Sidebar,
    layout_manager: LayoutManager,
    theme_editor: ThemeEditor,
    network_editor: NetworkEditor,
    data_sources: Arc<DataSources>,
    audio_stream: AudioStream,
    settings: modal::settings::Settings,
    theme: ThemeState,
    notifications: Notifications,
    alert_center: alerts::AlertCenter,
    alert_history: data::config::alert_history::AlertHistory,
    alert_history_stamp: Option<(std::time::SystemTime, u64)>,
    /// Saved alerts (alerts.json), shared with `orbit-tape watch` and the MCP.
    alert_store: data::config::alert_store::AlertStore,
    alert_store_stamp: Option<(std::time::SystemTime, u64)>,
    /// True while this app runs the saved alerts (holds the engine lock).
    alert_engine_owner: Arc<std::sync::atomic::AtomicBool>,
    ntfy_reports: alert_channels::Reports,
    last_alert_poll: std::time::Instant,
    window_scale_factors: window::WindowScaleFactors,
    network: Deferred<data::Network>,
}

#[derive(Debug, Clone)]
enum Message {
    Sidebar(screen::sidebar::Message),
    MarketWsEvent(exchange::Event),
    Dashboard {
        /// If `None`, the active layout is used for the event.
        layout_id: Option<uuid::Uuid>,
        event: dashboard::Message,
    },
    Tick(std::time::Instant),
    WindowEvent(window::Event),
    ExitRequested(HashMap<window::Id, WindowSpec>),
    SaveStateRequested(HashMap<window::Id, WindowSpec>),
    GoBack,
    RemoveNotification(usize),
    WindowScaleFactorUpdated {
        window: window::Id,
        scale_factor: window::WindowScaleFactor,
    },
    ThemeEditor(modal::theme_editor::Message),
    Settings(modal::settings::Message),
    NetworkEditor(modal::network_editor::Message),
    Layouts(modal::layout_manager::Message),
    SharedChartResources(manifest::Message),
    AudioStream(modal::audio::Message),
    /// Periodic check for COSMIC theme changes.
    PollCosmicTheme,
    AlertHistory(modal::alert_history::Message),
    /// Indicator source files changed on disk (debounced).
    IndicatorFilesChanged,
    IndicatorsReloaded(Result<data::chart::kline::indicators::IndicatorReloadReport, String>),
}

impl OrbitTape {
    fn new(boot: data::BootSettings) -> (Self, Task<Message>) {
        let saved_state = layout::load_saved_state();

        let (main_window_id, open_main_window) = {
            let (position, size) = saved_state.window();
            let config = window::Settings {
                size,
                position,
                exit_on_close_request: false,
                ..window::settings()
            };
            window::open(config)
        };

        let data_sources = Arc::new(DataSources::new(&saved_state.network));

        let (sidebar, launch_sidebar) = screen::sidebar::Sidebar::new(
            &saved_state.sidebar,
            saved_state.cache_market_metadata,
            data_sources.exchange.clone(),
        );

        let (audio_stream, audio_init_err) = AudioStream::new(saved_state.audio_cfg);

        let alert_store = data::config::alert_store::AlertStore::load();
        alert_channels::set_proxy(saved_state.network.proxy.clone());
        let alert_engine_owner = Arc::new(std::sync::atomic::AtomicBool::new(false));
        alert_engine::spawn_in_app(Arc::clone(&alert_engine_owner));
        let settings = modal::settings::Settings::new(
            saved_state.timezone,
            saved_state.scale_factor,
            saved_state.volume_size_unit,
            boot,
            saved_state.alerts,
            alert_store.channels.ntfy.clone(),
            data::chart::kline::indicators::manifest::normalize_catalog_setting(
                &saved_state.indicator_catalog_url,
            ),
        );
        let network = Deferred::new(saved_state.network);

        let alert_history = data::config::alert_history::AlertHistory::load();
        let mut state = Self {
            main_window: window::Window::new(main_window_id),
            layout_manager: saved_state.layout_manager,
            theme_editor: ThemeEditor::new(saved_state.custom_theme),
            window_scale_factors: window::WindowScaleFactors::new(main_window_id),
            audio_stream,
            sidebar,
            data_sources,
            settings,
            theme: ThemeState::new(
                saved_state.theme,
                saved_state.theme_source,
                saved_state.glass,
            )
            .with_accent_overrides(saved_state.accent_colors),
            notifications: Notifications::new(),
            alert_center: alerts::AlertCenter::new(&alert_history, exchange::UnixMs::now()),
            alert_history,
            alert_history_stamp: data::config::shared_file::modified(
                &data::config::alert_history::AlertHistory::path(),
            ),
            alert_store_stamp: data::config::shared_file::modified(
                &data::config::alert_store::AlertStore::path(),
            ),
            alert_store,
            alert_engine_owner,
            ntfy_reports: alert_channels::Reports::default(),
            last_alert_poll: std::time::Instant::now(),
            network_editor: NetworkEditor::new(&network),
            network,
        };

        for diagnostic in data::chart::kline::indicators::catalog().diagnostics() {
            state.notifications.push(Toast::warn(format!(
                "Indicator source quarantined: {diagnostic}"
            )));
        }

        if let Some(err) = audio_init_err {
            state
                .notifications
                .push(Toast::error(format!("Audio disabled: {err}")));
        }

        if state.layout_manager.layouts.is_empty() {
            log::error!("No layouts available after loading state; creating a default layout");
            state.layout_manager.ensure_layout();
        }

        let active_layout_id = state
            .layout_manager
            .active_layout_id()
            .or_else(|| {
                state
                    .layout_manager
                    .layouts
                    .first()
                    .map(|layout| &layout.id)
            })
            .map(|layout| layout.unique);

        let load_layout = active_layout_id
            .map(|uid| state.load_layout(uid, main_window_id))
            .unwrap_or_else(|| {
                log::error!("No active layout could be selected at startup");
                Task::none()
            });

        let open_main_window = open_main_window.then(|window_id| {
            window::scale_factor(window_id, |window, scale_factor| {
                Message::WindowScaleFactorUpdated {
                    window,
                    scale_factor,
                }
            })
        });

        (
            state,
            open_main_window
                .chain(load_layout)
                .chain(launch_sidebar.map(Message::Sidebar)),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::MarketWsEvent(event) => {
                let main_window_id = self.main_window.id;
                if self.layout_manager.active_layout_id().is_none() {
                    return Task::none();
                }
                let dashboard = self.active_dashboard_mut();

                match event {
                    exchange::Event::Connected(_streams) => {}
                    exchange::Event::Disconnected(_streams, reason) => {
                        log::info!("a stream disconnected from WS: {reason:?}");
                    }
                    exchange::Event::DepthReceived(stream, update_t, depth) => {
                        let task =
                            dashboard.ingest_live_depth(&stream, update_t, depth, main_window_id);
                        return self.map_dashboard_task(task, None);
                    }
                    exchange::Event::TradesReceived(stream, update_t, buffer) => {
                        let task = dashboard.ingest_live_trades(
                            &stream,
                            &buffer,
                            update_t,
                            main_window_id,
                        );

                        if let Some(msg) = self.audio_stream.try_play_sound(&stream, &buffer) {
                            self.notifications.push(Toast::error(msg));
                        }

                        return self.map_dashboard_task(task, None);
                    }
                    exchange::Event::KlineReceived(stream, kline) => {
                        let task = dashboard.ingest_live_kline(&stream, &kline, main_window_id);
                        return self.map_dashboard_task(task, None);
                    }
                }
            }
            Message::Tick(now) => {
                let main_window_id = self.main_window.id;
                let data_sources = Arc::clone(&self.data_sources);
                if self.layout_manager.active_layout_id().is_none() {
                    return Task::none();
                }

                let task = self
                    .active_dashboard_mut()
                    .tick(now, &data_sources, main_window_id);
                self.poll_alerts(now);
                return self.map_dashboard_task(task, None);
            }
            Message::WindowEvent(event) => return self.handle_window_event(event),
            Message::WindowScaleFactorUpdated {
                window,
                scale_factor,
            } => {
                self.window_scale_factors.set(window, scale_factor);
            }
            Message::ExitRequested(windows) => {
                self.save_state_to_disk(&windows);
                return iced::exit();
            }
            Message::SaveStateRequested(windows) => {
                self.save_state_to_disk(&windows);
            }
            Message::GoBack => {
                let main_window = self.main_window.id;

                if self.sidebar.active_menu().is_some() {
                    self.sidebar.set_menu(None);
                } else {
                    if self.active_dashboard_mut().go_back(main_window) {
                        return Task::none();
                    } else if self.active_dashboard().focus.is_some() {
                        self.active_dashboard_mut().clear_focus();
                    } else {
                        self.sidebar.hide_tickers_table();
                    }
                }
            }
            Message::Dashboard {
                layout_id: id,
                event: msg,
            } => {
                let Some(active_layout) = self.layout_manager.active_layout_id() else {
                    log::error!("No active layout to handle dashboard message");
                    return Task::none();
                };

                let main_window = self.main_window;
                let active_layout_id = active_layout.unique;
                let layout_id = id.unwrap_or(active_layout_id);

                let Some((main_task, event)) = self.layout_manager.update_dashboard(
                    msg,
                    &main_window,
                    &layout_id,
                    &self.data_sources,
                ) else {
                    return Task::none();
                };

                let additional_task = match event {
                    Some(dashboard::Event::OpenUrlRequested(url)) => {
                        self.open_url(&url);
                        Task::none()
                    }
                    Some(dashboard::Event::Notification(toast)) => {
                        self.notifications.push(toast);
                        Task::none()
                    }
                    Some(dashboard::Event::ResolveStreams { pane_id, streams }) => {
                        let tickers_info = self.sidebar.tickers_info();

                        let resolved_streams =
                            streams.into_iter().try_fold(vec![], |mut acc, persist| {
                                let resolver =
                                    |t: &exchange::Ticker| tickers_info.get(t).and_then(|opt| *opt);

                                match persist.into_stream_kinds(resolver) {
                                    Ok(mut resolved) => {
                                        acc.append(&mut resolved);
                                        Ok(acc)
                                    }
                                    Err(err) => Err(err),
                                }
                            });

                        match resolved_streams {
                            Ok(resolved) => {
                                if resolved.is_empty() {
                                    Task::none()
                                } else {
                                    let task = self
                                        .layout_manager
                                        .mut_dashboard(layout_id)
                                        .map(|dashboard| {
                                            dashboard.resolve_streams(
                                                main_window.id,
                                                pane_id,
                                                resolved,
                                            )
                                        })
                                        .unwrap_or_else(Task::none);
                                    self.map_dashboard_task(task, Some(layout_id))
                                }
                            }
                            Err(err) => {
                                if self.sidebar.is_metadata_loading() {
                                    // Metadata fetches are still in flight
                                    log::debug!(
                                        "Deferring stream resolution for pane {pane_id}: metadata still loading ({err})"
                                    );
                                } else if let Some(dashboard) =
                                    self.layout_manager.mut_dashboard(layout_id)
                                {
                                    log::debug!("Blocking streams for pane {pane_id}: {err}");
                                    dashboard.block_streams(
                                        main_window.id,
                                        pane_id,
                                        format!("Metadata not available: {err}"),
                                    );
                                }
                                Task::none()
                            }
                        }
                    }
                    Some(dashboard::Event::OpenNetworkSettings) => {
                        return self.update(Message::Sidebar(
                            screen::sidebar::Message::ToggleSidebarMenu(Some(
                                sidebar::Menu::Network,
                            )),
                        ));
                    }
                    Some(dashboard::Event::RequestPalette) => {
                        let theme = self.theme.current().clone();

                        if let Some(dashboard) = self.layout_manager.mut_dashboard(layout_id) {
                            dashboard.theme_updated(main_window.id, &theme);
                        }

                        Task::none()
                    }
                    Some(dashboard::Event::PresetCommand {
                        window,
                        pane,
                        command,
                    }) => {
                        self.layout_manager.apply_preset_command(
                            main_window.id,
                            layout_id,
                            window,
                            pane,
                            command,
                        );
                        Task::none()
                    }
                    Some(dashboard::Event::IndicatorCatalog(message)) => {
                        self.update_shared_chart_resources(message)
                    }
                    None => Task::none(),
                };

                return self
                    .map_dashboard_task(main_task, Some(layout_id))
                    .chain(additional_task);
            }
            Message::RemoveNotification(index) => {
                self.notifications.remove(index);
            }
            Message::SharedChartResources(message) => {
                return self.update_shared_chart_resources(message);
            }
            Message::Layouts(message) => {
                let action = self.layout_manager.update(message);

                match action {
                    Some(modal::layout_manager::Action::Select(layout)) => {
                        let active_popout_keys = self
                            .active_dashboard()
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<_>>();

                        let window_tasks = Task::batch(
                            active_popout_keys
                                .iter()
                                .map(|&popout_id| window::close::<window::Id>(popout_id))
                                .collect::<Vec<_>>(),
                        )
                        .discard();

                        let old_layout_id = self
                            .layout_manager
                            .active_layout_id()
                            .map(|layout| layout.unique)
                            .expect("No active layout");

                        return self
                            .map_dashboard_task(
                                window::collect_window_specs(
                                    active_popout_keys,
                                    dashboard::Message::SavePopoutSpecs,
                                ),
                                Some(old_layout_id),
                            )
                            .chain(window_tasks)
                            .chain(self.load_layout(layout, self.main_window.id));
                    }
                    Some(modal::layout_manager::Action::Clone(id)) => {
                        let manager = &mut self.layout_manager;

                        let source_data = manager.get(id).map(|layout| {
                            (
                                layout.id.name.clone(),
                                data::Dashboard::from(&layout.dashboard),
                            )
                        });

                        if let Some((name, ser_dashboard)) = source_data {
                            let new_uid = uuid::Uuid::new_v4();
                            let new_layout = LayoutId {
                                unique: new_uid,
                                name: manager.ensure_unique_name(&name, new_uid),
                            };

                            let mut popout_windows = Vec::new();

                            for (pane, window_spec) in &ser_dashboard.popout {
                                let configuration = configuration(pane.clone());
                                popout_windows.push((configuration, *window_spec));
                            }

                            let dashboard = Dashboard::from_config(
                                configuration(ser_dashboard.pane.clone()),
                                popout_windows,
                                ser_dashboard.chart_settings,
                            );

                            manager.insert_layout(new_layout.clone(), dashboard);
                        }
                    }
                    None => {}
                }
            }
            Message::AudioStream(message) => {
                if let Some(event) = self.audio_stream.update(message) {
                    match event {
                        modal::audio::UpdateEvent::RetryFailed(err) => {
                            self.notifications
                                .push(Toast::error(format!("Audio still unavailable: {err}")));
                        }
                        modal::audio::UpdateEvent::RetrySucceeded => {
                            self.notifications.push(Toast::info(
                                "Audio output re-initialized successfully".to_string(),
                            ));
                        }
                    }
                }
            }
            Message::PollCosmicTheme => {
                let changed = self.theme.poll_cosmic();
                self.theme_changed(changed);
                self.sync_blur();
            }
            Message::IndicatorFilesChanged => {
                return Task::perform(
                    async { data::chart::kline::indicators::reload_indicator_catalog() },
                    Message::IndicatorsReloaded,
                );
            }
            Message::IndicatorsReloaded(result) => self.indicators_reloaded(result),
            Message::AlertHistory(message) => return self.update_alert_history(message),
            Message::ThemeEditor(msg) => {
                let action = self.theme_editor.update(msg, &self.theme);

                match action {
                    Some(modal::theme_editor::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    Some(modal::theme_editor::Action::UpdateTheme(theme)) => {
                        self.select_theme(theme);
                    }
                    Some(modal::theme_editor::Action::SetAccent(index, color)) => {
                        let changed = self.theme.set_accent(index, color);
                        self.theme_changed(changed);
                    }
                    None => {}
                }
            }
            Message::Settings(message) => {
                use modal::settings::Action;

                match self.settings.update(message) {
                    Some(Action::Persist) => return self.save_now(),
                    Some(Action::ThemeSelected(theme)) => {
                        self.theme_editor.reset_input();
                        self.select_theme(theme);
                    }
                    Some(Action::EditAccent(index)) => {
                        self.theme_editor.focus_accent(index);
                        self.sidebar.set_menu(Some(sidebar::Menu::ThemeEditor));
                    }
                    Some(Action::SetThemeSource(source)) => {
                        let changed = self.theme.set_source(source);
                        self.theme_changed(changed);
                    }
                    Some(Action::Glass(change)) => {
                        let mut glass = self.theme.glass();
                        change.apply(&mut glass);
                        self.theme.set_glass(glass);
                        self.sync_blur();
                    }
                    Some(Action::SetMetadataCaching(enabled)) => {
                        self.sidebar.set_cache_enabled(enabled);
                    }
                    Some(Action::RefreshMetadata) => {
                        if let Some(task) = self.sidebar.force_refresh_metadata() {
                            return task.map(Message::Sidebar);
                        }
                    }
                    Some(Action::OpenDataFolder) => {
                        if let Err(err) = data::open_data_folder() {
                            self.notifications
                                .push(Toast::error(format!("Failed to open data folder: {err}")));
                        }
                    }
                    Some(Action::OpenUrl(url)) => self.open_url(url.as_ref()),
                    Some(Action::TestAlert) => {
                        if let Some(err) = self.alert_center.test(&self.settings.alerts) {
                            self.notifications.push(Toast::error(err));
                        }
                    }
                    Some(Action::SaveNtfy(ntfy)) => {
                        let saved = ntfy.clone();
                        self.edit_saved_alert(|store| {
                            store.channels.ntfy = ntfy;
                            Ok(())
                        });
                        if self.alert_store.channels.ntfy == saved {
                            self.settings.ntfy_saved = saved;
                        }
                    }
                    Some(Action::TestNtfy) => {
                        alert_channels::spawn_ntfy(
                            alert_channels::Outgoing {
                                summary: "Orbit Tape test alert".to_owned(),
                                body: "ntfy works. Alerts will look like this.".to_owned(),
                            },
                            self.alert_store.channels.ntfy.clone(),
                            self.ntfy_reports
                                .recorder(Some("Test message sent to ntfy".to_owned())),
                        );
                    }
                    Some(Action::Sidebar(message)) => {
                        return self.update(Message::Sidebar(message));
                    }
                    None => {}
                }
            }
            Message::NetworkEditor(msg) => {
                let action = self.network_editor.update(msg);

                match action {
                    Some(network_editor::Action::ApplyProxy(ref proxy)) => {
                        if let Some(proxy) = proxy {
                            data::config::auth::save_proxy_auth(proxy);
                        } else if let Some(ref old_proxy) = self.network.wanted().proxy {
                            data::config::auth::delete_proxy_auth(old_proxy);
                        }

                        self.network.set_wanted(data::Network {
                            proxy: proxy.clone(),
                            ..self.network.wanted().clone()
                        });

                        return self.save_now();
                    }
                    Some(network_editor::Action::ApplyServerConfig {
                        mode,
                        url,
                        auth_token,
                        accept_invalid_certs,
                    }) => {
                        if let Some(ref url) = url
                            && let Some(ref token) = auth_token
                        {
                            data::config::auth::save_server_token(url, token);
                        } else if let Some(ref old_url) = self.network.wanted().server_url {
                            data::config::auth::delete_server_token(old_url);
                        }

                        self.network.set_wanted(data::Network {
                            server_url: url,
                            server_auth_token: auth_token,
                            trade_fetch_mode: mode,
                            server_accept_invalid_certs: accept_invalid_certs,
                            ..self.network.wanted().clone()
                        });

                        return self.save_now();
                    }
                    Some(network_editor::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    None => {}
                }
            }
            Message::Sidebar(message) => {
                let (task, action) = self.sidebar.update(message);

                match action {
                    Some(screen::sidebar::Action::MenuChanged(Some(sidebar::Menu::Network))) => {
                        self.network_editor.reopen(&self.network);
                    }
                    Some(screen::sidebar::Action::MenuChanged(Some(sidebar::Menu::Alerts)))
                        if self.alert_history.unread() > 0 =>
                    {
                        self.mark_alerts_read();
                    }
                    Some(screen::sidebar::Action::MenuChanged(_)) => {}
                    Some(screen::sidebar::Action::TickerSelected(ticker_info, content)) => {
                        let main_window_id = self.main_window.id;
                        let data_sources = Arc::clone(&self.data_sources);
                        if self.layout_manager.active_layout_id().is_none() {
                            return task.map(Message::Sidebar);
                        }

                        let dashboard_task = {
                            if let Some(kind) = content {
                                self.active_dashboard_mut().init_focused_pane(
                                    &data_sources,
                                    main_window_id,
                                    ticker_info,
                                    kind,
                                )
                            } else {
                                self.active_dashboard_mut().switch_tickers_in_group(
                                    &data_sources,
                                    main_window_id,
                                    ticker_info,
                                )
                            }
                        };

                        return self.map_dashboard_task(dashboard_task, None);
                    }
                    Some(screen::sidebar::Action::ErrorOccurred(err)) => {
                        self.notifications.push(Toast::error(err.to_string()));
                    }
                    None => {}
                }

                return task.map(Message::Sidebar);
            }
        }
        Task::none()
    }

    /// Live reload finished: refresh every chart and say what happened.
    fn indicators_reloaded(
        &mut self,
        result: Result<data::chart::kline::indicators::IndicatorReloadReport, String>,
    ) {
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                self.notifications
                    .push(Toast::error(format!("Indicator reload failed: {error}")));
                return;
            }
        };
        if !report.reloaded.is_empty() || !report.removed.is_empty() {
            for layout in &mut self.layout_manager.layouts {
                layout.dashboard.indicator_catalog_reloaded();
            }
        }
        if !report.reloaded.is_empty() {
            self.notifications.push(Toast::info(format!(
                "Reloaded {}",
                report.reloaded.join(", ")
            )));
        }
        if !report.removed.is_empty() {
            self.notifications.push(Toast::info(format!(
                "Indicator removed: {}",
                report.removed.join(", ")
            )));
        }
        for error in report.errors {
            self.notifications.push(Toast::error(error));
        }
    }

    /// Collects alert conditions from the charts and delivers them, keeps
    /// the charts' price alert lines in step with the saved alerts, and
    /// picks up what other programs saved. Runs at most twice a second; bar
    /// closes are rarer than that.
    fn poll_alerts(&mut self, now: std::time::Instant) {
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
        if now.saturating_duration_since(self.last_alert_poll) < INTERVAL {
            return;
        }
        self.last_alert_poll = now;
        self.reload_shared_alerts();
        for report in self.ntfy_reports.take() {
            self.notifications.push(match report {
                Ok(text) => Toast::info(text),
                Err(error) => Toast::error(error),
            });
        }

        let unix_now = exchange::UnixMs::now();
        let store = std::mem::take(&mut self.alert_store);
        let edits = self
            .active_dashboard_mut()
            .sync_price_alerts(&store, unix_now);
        self.alert_store = store;
        if !edits.is_empty() {
            self.apply_store_edits(edits);
        }

        let hits = self.active_dashboard_mut().take_alerts(unix_now);
        if hits.is_empty() {
            return;
        }
        let delivery = self.alert_center.deliver(
            hits,
            &self.settings.alerts,
            &self.alert_store.channels.ntfy,
            &self.ntfy_reports,
        );
        if let Some(history) = delivery.history {
            self.alert_history = history;
        }
        // The list is open, so the user sees the new entries.
        if !delivery.fired.is_empty() && self.sidebar.is_menu_active(sidebar::Menu::Alerts) {
            self.mark_alerts_read();
        }
        if let Some(err) = delivery.error {
            self.notifications.push(Toast::error(err));
        }
    }

    /// Reloads alerts.json and the alert history when another program (the
    /// alert engine, `orbit-tape watch`, the MCP) changed them. New fired
    /// alerts from elsewhere play the chime.
    fn reload_shared_alerts(&mut self) {
        use data::config::alert_history::AlertHistory;
        use data::config::alert_store::AlertStore;
        use data::config::shared_file;

        let stamp = shared_file::modified(&AlertStore::path());
        if stamp != self.alert_store_stamp {
            self.alert_store_stamp = stamp;
            self.alert_store = AlertStore::load();
        }

        let stamp = shared_file::modified(&AlertHistory::path());
        if stamp == self.alert_history_stamp {
            return;
        }
        self.alert_history_stamp = stamp;
        let loaded = AlertHistory::load();
        let new = loaded
            .entries()
            .filter(|entry| !self.alert_history.entries().any(|known| known == *entry))
            .count();
        self.alert_history = loaded;
        if new == 0 {
            return;
        }
        if self.sidebar.is_menu_active(sidebar::Menu::Alerts) {
            self.mark_alerts_read();
        }
        if let Some(err) = self.alert_center.chime(&self.settings.alerts) {
            self.notifications.push(Toast::error(err));
        }
    }

    /// Saves what the charts changed on their alert lines.
    fn apply_store_edits(&mut self, edits: Vec<screen::dashboard::pane::StoreEdit>) {
        use data::config::alert_store::{AlertKind, AlertStore};
        use screen::dashboard::pane::StoreEdit;

        let saved = AlertStore::update(|store| {
            let mut errors = Vec::new();
            for edit in edits {
                match edit {
                    StoreEdit::Add(alert) => {
                        if let Err(error) = store.add(alert.clone()) {
                            // Over the limit: keep it, paused.
                            errors.push(error);
                            let _ = store.add(data::config::alert_store::StoredAlert {
                                enabled: false,
                                ..alert
                            });
                        }
                    }
                    StoreEdit::Update { id, level, enabled } => {
                        if let (Some(new), Some(AlertKind::Price { level, .. })) =
                            (level, store.get_mut(&id).map(|alert| &mut alert.kind))
                        {
                            *level = new;
                        }
                        if let Some(enabled) = enabled
                            && let Err(error) = store.set_enabled(&id, enabled)
                        {
                            errors.push(error);
                        }
                    }
                    StoreEdit::Delete(id) => {
                        store.remove(&id);
                    }
                }
            }
            (store.clone(), errors)
        });
        match saved {
            Ok((store, errors)) => {
                self.alert_store = store;
                for error in errors {
                    self.notifications.push(Toast::warn(error));
                }
            }
            Err(error) => self
                .notifications
                .push(Toast::error(format!("Could not save the alerts: {error}"))),
        }
    }

    /// Changes one saved alert from the alert list.
    fn edit_saved_alert(
        &mut self,
        change: impl FnOnce(&mut data::config::alert_store::AlertStore) -> Result<(), String>,
    ) {
        match data::config::alert_store::AlertStore::update(|store| {
            let result = change(store);
            (store.clone(), result)
        }) {
            Ok((store, result)) => {
                self.alert_store = store;
                if let Err(error) = result {
                    self.notifications.push(Toast::warn(error));
                }
            }
            Err(error) => self
                .notifications
                .push(Toast::error(format!("Could not save the alerts: {error}"))),
        }
    }

    fn mark_alerts_read(&mut self) {
        self.alert_history.mark_read();
        match data::config::alert_history::AlertHistory::mark_read_saved() {
            Ok(history) => self.alert_history = history,
            Err(error) => log::warn!("could not save the alert history: {error}"),
        }
    }

    fn update_alert_history(&mut self, message: modal::alert_history::Message) -> Task<Message> {
        match message {
            modal::alert_history::Message::SetEnabled(id, enabled) => {
                self.edit_saved_alert(|store| store.set_enabled(&id, enabled));
                Task::none()
            }
            modal::alert_history::Message::SetFrequency(id, frequency) => {
                self.edit_saved_alert(|store| {
                    let alert = store.get_mut(&id).ok_or("That alert was deleted")?;
                    alert.frequency = frequency;
                    Ok(())
                });
                Task::none()
            }
            modal::alert_history::Message::SetChannel(id, channel) => {
                let ntfy_missing = channel.ntfy() && !self.alert_store.channels.ntfy.is_set();
                self.edit_saved_alert(|store| {
                    let alert = store.get_mut(&id).ok_or("That alert was deleted")?;
                    alert.channel = channel;
                    Ok(())
                });
                if ntfy_missing {
                    self.notifications.push(Toast::warn(
                        "Set an ntfy topic in Settings, Alerts first".to_owned(),
                    ));
                }
                Task::none()
            }
            modal::alert_history::Message::Delete(id) => {
                self.edit_saved_alert(|store| {
                    store.remove(&id);
                    Ok(())
                });
                Task::none()
            }
            modal::alert_history::Message::Clear => {
                self.alert_history.clear();
                match data::config::alert_history::AlertHistory::clear_saved() {
                    Ok(history) => self.alert_history = history,
                    Err(error) => log::warn!("could not save the alert history: {error}"),
                }
                Task::none()
            }
            modal::alert_history::Message::Focus(index) => {
                let Some(alert) = self.alert_history.get(index).cloned() else {
                    return Task::none();
                };
                let main_window = self.main_window.id;
                match self.active_dashboard_mut().focus_chart(
                    main_window,
                    alert.ticker,
                    alert.basis,
                ) {
                    Some(window) => {
                        self.sidebar.set_menu(None);
                        iced::window::gain_focus(window)
                    }
                    None => {
                        self.notifications.push(Toast::warn(format!(
                            "No chart for {} is open in this layout",
                            alert.symbol
                        )));
                        Task::none()
                    }
                }
            }
        }
    }

    fn save_now(&self) -> Task<Message> {
        let mut active_windows: Vec<window::Id> =
            self.active_dashboard().popout.keys().copied().collect();
        active_windows.push(self.main_window.id);

        window::collect_window_specs(active_windows, Message::SaveStateRequested)
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        let dashboard = self.active_dashboard();
        let sidebar_pos = self.sidebar.position();

        let pixel_ratio = self
            .window_scale_factors
            .pixel_ratio_for_window(id, self.settings.scale_factor);

        let tickers_table = &self.sidebar.tickers_table;

        let content = if id == self.main_window.id {
            let sidebar_view = self
                .sidebar
                .view(self.audio_stream.volume(), self.alert_history.unread())
                .map(Message::Sidebar);
            let dashboard_view = dashboard
                .view(
                    &self.main_window,
                    tickers_table,
                    self.settings.timezone,
                    pixel_ratio,
                    &self.theme,
                    self.layout_manager.chart_resources(),
                )
                .map(move |msg| Message::Dashboard {
                    layout_id: None,
                    event: msg,
                });

            let header_title = { column![] };

            let dashboard_row = match sidebar_pos {
                sidebar::Position::Left => row![sidebar_view, dashboard_view],
                sidebar::Position::Right => row![dashboard_view, sidebar_view],
            };

            let base = column![header_title, dashboard_row.spacing(4).padding(8),];

            if let Some(menu) = self.sidebar.active_menu() {
                self.view_with_modal(base.into(), dashboard, menu)
            } else {
                base.into()
            }
        } else {
            container(
                dashboard
                    .view_window(
                        id,
                        &self.main_window,
                        tickers_table,
                        self.settings.timezone,
                        pixel_ratio,
                        &self.theme,
                        self.layout_manager.chart_resources(),
                    )
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    }),
            )
            .into()
        };

        toast::Manager::new(
            content,
            self.notifications.toasts(),
            match sidebar_pos {
                sidebar::Position::Left => Alignment::Start,
                sidebar::Position::Right => Alignment::End,
            },
            Message::RemoveNotification,
        )
        .into()
    }

    fn theme(&self, _window: window::Id) -> iced_core::Theme {
        self.theme.current().clone()
    }

    /// The window background, see-through when glass is on.
    fn style(&self, theme: &iced_core::Theme) -> iced::theme::Style {
        let palette = theme.extended_palette();
        iced::theme::Style {
            background_color: palette
                .background
                .base
                .color
                .scale_alpha(self.theme.window_opacity()),
            text_color: palette.background.base.text,
        }
    }

    fn title(&self, _window: window::Id) -> String {
        if let Some(id) = self.layout_manager.active_layout_id() {
            format!("OrbitTape [{}]", id.name)
        } else {
            "Orbit Tape".to_string()
        }
    }

    fn scale_factor(&self, _window: window::Id) -> f32 {
        self.settings.scale_factor.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let window_events = window::events().map(Message::WindowEvent);
        let sidebar = self.sidebar.subscription().map(Message::Sidebar);

        let exchange_streams = self
            .active_dashboard()
            .market_subscriptions(&self.data_sources.exchange)
            .map(Message::MarketWsEvent);

        let tick = iced::window::frames().map(Message::Tick);

        let hotkeys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, .. } = event else {
                return None;
            };
            match key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::GoBack),
                _ => None,
            }
        });

        // COSMIC rewrites its theme files on a change; polling their mtimes is
        // cheap and needs no file watcher.
        let cosmic_theme = if self.theme.polls_cosmic() {
            iced::time::every(COSMIC_THEME_POLL_INTERVAL).map(|_| Message::PollCosmicTheme)
        } else {
            Subscription::none()
        };

        let indicator_files =
            indicator_watch::subscription().map(|()| Message::IndicatorFilesChanged);

        Subscription::batch(vec![
            exchange_streams,
            sidebar,
            window_events,
            tick,
            hotkeys,
            cosmic_theme,
            indicator_files,
        ])
    }

    fn active_dashboard(&self) -> &Dashboard {
        let active_layout = self
            .layout_manager
            .active_layout_id()
            .expect("No active layout");
        self.layout_manager
            .get(active_layout.unique)
            .map(|layout| &layout.dashboard)
            .expect("No active dashboard")
    }

    fn active_dashboard_mut(&mut self) -> &mut Dashboard {
        let active_layout = self
            .layout_manager
            .active_layout_id()
            .expect("No active layout");
        self.layout_manager
            .get_mut(active_layout.unique)
            .map(|layout| &mut layout.dashboard)
            .expect("No active dashboard")
    }

    fn update_shared_chart_resources(&mut self, message: manifest::Message) -> Task<Message> {
        self.layout_manager
            .update_chart_resources(
                message,
                &self.data_sources.catalog_http,
                &data::chart::kline::indicators::manifest::resolve_catalog_url(
                    &self.settings.indicator_catalog_url,
                ),
            )
            .map(Message::SharedChartResources)
    }

    fn map_dashboard_task(
        &self,
        task: Task<dashboard::Message>,
        layout_id: Option<uuid::Uuid>,
    ) -> Task<Message> {
        let Some(layout_id) = layout_id.or_else(|| {
            self.layout_manager
                .active_layout_id()
                .map(|layout| layout.unique)
        }) else {
            return Task::none();
        };

        task.map(move |event| Message::Dashboard {
            layout_id: Some(layout_id),
            event,
        })
    }

    fn open_url(&mut self, url: &str) {
        if let Err(err) = data::open_url(url) {
            self.notifications
                .push(Toast::error(format!("Failed to open link: {err}")));
        }
    }

    /// Picks a built-in or custom theme, which stops following COSMIC.
    fn select_theme(&mut self, theme: iced_core::Theme) {
        let mut changed = self.theme.select(theme);
        if self.theme.source().follows_cosmic() {
            changed |= self.theme.set_source(ThemeSource::Selected);
        }
        self.theme_changed(changed);
    }

    /// Blurs what is behind the windows while their background is see-through.
    fn sync_blur(&self) {
        blur::set_all(self.theme.window_opacity() < 1.0);
    }

    /// Pushes the effective theme to the panes after it changed.
    fn theme_changed(&mut self, changed: bool) {
        if !changed {
            return;
        }

        let theme = self.theme.current().clone();
        let main_window = self.main_window.id;
        self.active_dashboard_mut()
            .theme_updated(main_window, &theme);
    }

    fn handle_window_event(&mut self, event: window::Event) -> Task<Message> {
        match event {
            window::Event::Opened(window_id) => {
                let scale_factor = window::scale_factor(window_id, |window, scale_factor| {
                    Message::WindowScaleFactorUpdated {
                        window,
                        scale_factor,
                    }
                });
                let blur = {
                    let blurred = self.theme.window_opacity() < 1.0;
                    iced::window::run(window_id, move |window| {
                        blur::attach(window_id, window, blurred);
                    })
                    .discard()
                };
                Task::batch([scale_factor, blur])
            }
            window::Event::Closed(window_id) => {
                blur::detach(window_id);
                Task::none()
            }
            window::Event::Rescaled(window_id, scale_factor) => {
                self.window_scale_factors.set(window_id, scale_factor);
                Task::none()
            }
            window::Event::CloseRequested(window_id) => {
                let main_window = self.main_window.id;

                if window_id != main_window {
                    self.window_scale_factors.remove(window_id);
                    self.active_dashboard_mut().popout.remove(&window_id);
                    return window::close(window_id);
                }

                let dashboard = self.active_dashboard_mut();

                let mut active_windows = dashboard
                    .popout
                    .keys()
                    .copied()
                    .collect::<Vec<window::Id>>();
                active_windows.push(main_window);

                window::collect_window_specs(active_windows, Message::ExitRequested)
            }
        }
    }

    fn load_layout(&mut self, layout_uid: uuid::Uuid, main_window: window::Id) -> Task<Message> {
        if let Err(err) = self.layout_manager.set_active_layout(layout_uid) {
            log::error!("Failed to set active layout: {}", err);
            return Task::none();
        }

        self.layout_manager
            .park_inactive_layouts(layout_uid, main_window);

        let task = self
            .layout_manager
            .get_mut(layout_uid)
            .map(|layout| layout.dashboard.load_layout(main_window));

        task.map(|task| self.map_dashboard_task(task, Some(layout_uid)))
            .unwrap_or_else(|| {
                log::error!("Active layout missing after selection: {}", layout_uid);
                Task::none()
            })
    }

    fn view_with_modal<'a>(
        &'a self,
        base: Element<'a, Message>,
        dashboard: &'a Dashboard,
        menu: sidebar::Menu,
    ) -> Element<'a, Message> {
        let sidebar_pos = self.sidebar.position();

        match menu {
            sidebar::Menu::Settings => {
                let settings_modal = self
                    .settings
                    .view(
                        &self.theme,
                        self.theme_editor.custom_theme.as_ref(),
                        &self.sidebar,
                        sidebar_pos,
                    )
                    .map(Message::Settings);

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    settings_modal,
                    Message::Sidebar(screen::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::Layout => {
                let main_window = self.main_window.id;

                let manage_pane = if let Some((window_id, pane_id)) = dashboard.focus {
                    let selected_pane_str =
                        if let Some(state) = dashboard.get_pane(main_window, window_id, pane_id) {
                            let link_group_name: String =
                                state.link_group.as_ref().map_or_else(String::new, |g| {
                                    " - Group ".to_string() + &g.to_string()
                                });

                            state.content.to_string() + &link_group_name
                        } else {
                            "".to_string()
                        };

                    let is_main_window = window_id == main_window;

                    let reset_pane_button = {
                        let btn = button(text("Reset").align_x(Alignment::Center))
                            .width(iced::Length::Fill);
                        if is_main_window {
                            let dashboard_msg = Message::Dashboard {
                                layout_id: None,
                                event: dashboard::Message::Pane(
                                    main_window,
                                    dashboard::pane::Message::ReplacePane(pane_id),
                                ),
                            };

                            btn.on_press(dashboard_msg)
                        } else {
                            btn
                        }
                    };
                    let split_pane_button = {
                        let btn = button(text("Split").align_x(Alignment::Center))
                            .width(iced::Length::Fill);
                        if is_main_window {
                            let dashboard_msg = Message::Dashboard {
                                layout_id: None,
                                event: dashboard::Message::Pane(
                                    main_window,
                                    dashboard::pane::Message::SplitPane(
                                        pane_grid::Axis::Horizontal,
                                        pane_id,
                                    ),
                                ),
                            };
                            btn.on_press(dashboard_msg)
                        } else {
                            btn
                        }
                    };

                    column![
                        text(selected_pane_str),
                        row![
                            tooltip(
                                reset_pane_button,
                                if is_main_window {
                                    Some("Reset selected pane")
                                } else {
                                    None
                                },
                                TooltipPosition::Top,
                            ),
                            tooltip(
                                split_pane_button,
                                if is_main_window {
                                    Some("Split selected pane horizontally")
                                } else {
                                    None
                                },
                                TooltipPosition::Top,
                            ),
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                } else {
                    let reset_pane_button =
                        button(text("Reset").align_x(Alignment::Center)).width(iced::Length::Fill);
                    let split_pane_button =
                        button(text("Split").align_x(Alignment::Center)).width(iced::Length::Fill);

                    column![
                        text("No pane selected"),
                        row![
                            tooltip(reset_pane_button, None, TooltipPosition::Top),
                            tooltip(split_pane_button, None, TooltipPosition::Top),
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                };

                let manage_layout_modal = {
                    let col = column![
                        manage_pane,
                        rule::horizontal(1.0).style(style::split_ruler),
                        self.layout_manager.view().map(Message::Layouts)
                    ]
                    .spacing(20);

                    container(col.align_x(Alignment::Center))
                        .width(260)
                        .padding(24)
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(40)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(40)),
                };

                dashboard_modal(
                    base,
                    manage_layout_modal,
                    Message::Sidebar(screen::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::Audio => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(76)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(76)),
                };

                let trade_streams_list = dashboard.streams.trade_streams(None);

                dashboard_modal(
                    base,
                    self.audio_stream
                        .view(trade_streams_list)
                        .map(Message::AudioStream),
                    Message::Sidebar(screen::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::Alerts => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(108)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(108)),
                };

                dashboard_modal(
                    base,
                    modal::alert_history::view(
                        &self.alert_history,
                        &self.alert_store,
                        if self
                            .alert_engine_owner
                            .load(std::sync::atomic::Ordering::Relaxed)
                        {
                            modal::alert_history::Runner::ThisApp
                        } else {
                            modal::alert_history::Runner::Elsewhere
                        },
                    )
                    .map(Message::AlertHistory),
                    Message::Sidebar(screen::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::ThemeEditor => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    self.theme_editor
                        .view(&self.theme)
                        .map(Message::ThemeEditor),
                    Message::Sidebar(screen::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::Network => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    self.network_editor
                        .view(&self.network)
                        .map(Message::NetworkEditor),
                    Message::Sidebar(screen::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
        }
    }

    fn save_state_to_disk(&mut self, windows: &HashMap<window::Id, WindowSpec>) {
        self.active_dashboard_mut()
            .popout
            .iter_mut()
            .for_each(|(id, (_, window_spec))| {
                if let Some(new_window_spec) = windows.get(id) {
                    *window_spec = *new_window_spec;
                }
            });

        self.sidebar.sync_tickers_table_settings();

        let mut ser_layouts = vec![];
        for layout in &self.layout_manager.layouts {
            if let Some(layout) = self.layout_manager.get(layout.id.unique) {
                let serialized_dashboard = data::Dashboard::from(&layout.dashboard);
                ser_layouts.push(data::Layout {
                    name: layout.id.name.clone(),
                    dashboard: serialized_dashboard,
                });
            }
        }

        let layouts = data::Layouts {
            chart_presets: self.layout_manager.presets().entries().to_vec(),
            layouts: ser_layouts,
            active_layout: self
                .layout_manager
                .active_layout_id()
                .map(|layout| layout.name.to_string())
                .clone(),
        };

        let main_window_spec = windows
            .iter()
            .find(|(id, _)| **id == self.main_window.id)
            .map(|(_, spec)| *spec);

        let audio_cfg = data::AudioStream::from(&self.audio_stream);

        let state = data::State::from_parts(
            layouts,
            self.theme.data_theme(),
            self.theme_editor.custom_theme.clone().map(data::Theme),
            self.theme.source(),
            self.theme.glass(),
            main_window_spec,
            self.settings.timezone,
            self.sidebar.state.clone(),
            self.settings.scale_factor,
            audio_cfg,
            self.settings.alerts.clone(),
            self.network.wanted().for_persistence(),
            *self.settings.volume_size_unit.wanted(),
            self.sidebar.cache_enabled(),
            *self.settings.boot.wanted(),
        );

        let state = data::State {
            indicator_catalog_url: self.settings.indicator_catalog_url.trim().to_owned(),
            accent_colors: self
                .theme
                .accent_overrides()
                .map(|color| color.map(data::config::theme::color_to_hex)),
            ..state
        };

        match serde_json::to_string(&state) {
            Ok(layout_str) => {
                let file_name = data::SAVED_STATE_PATH;
                if let Err(e) = data::write_json_to_file(&layout_str, file_name) {
                    log::error!("Failed to write layout state to file: {}", e);
                } else {
                    log::info!("Persisted state to {file_name}");
                }
            }
            Err(e) => log::error!("Failed to serialize layout: {}", e),
        }

        self.sidebar.persist_metadata_cache();
    }
}
