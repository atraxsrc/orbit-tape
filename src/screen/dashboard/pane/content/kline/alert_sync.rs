//! Keeps a chart's price alert lines and the saved price alerts
//! (`alerts.json`) in step, both ways.
//!
//! - A line with an alert and no saved alert yet (a new one, or one from a
//!   layout saved before alerts were shared) is saved as an alert that fires
//!   once on a cross either way.
//! - Moving, arming or disarming a line changes its saved alert; a line
//!   deleted on the chart deletes it.
//! - A saved alert changed elsewhere (the alert list, Claude, the engine
//!   turning off a `once` alert that fired) moves or re-arms the line; one
//!   deleted elsewhere removes the line.
//! - A saved price alert for this chart's market with no line gets one.
//!
//! The chart remembers the level and state it last agreed with the store,
//! so it can tell which side changed.

use super::drawing::DrawingTools;
use data::chart::Basis;
use data::chart::kline::YUnit;
use data::chart::kline::drawing::DrawingStyle;
use data::config::alert_store::{
    AlertKind, AlertStore, Channel, Creator, Frequency, PriceCross, StoredAlert,
};
use exchange::unit::price::Price;
use exchange::{Ticker, TickerInfo, Timeframe, UnixMs};
use std::collections::{HashMap, HashSet};

/// A change to the saved alerts that a chart asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreEdit {
    Add(StoredAlert),
    Update {
        id: String,
        level: Option<f64>,
        enabled: Option<bool>,
    },
    Delete(String),
}

#[derive(Debug, Default)]
pub struct AlertSync {
    ticker: Option<Ticker>,
    /// Level and armed state last agreed with the store, by alert ID.
    synced: HashMap<String, (YUnit, bool)>,
    /// Deleted from this chart; the store snapshot may still list them.
    deleted: HashSet<String>,
}

pub fn level_of(y_unit: YUnit, ticker_info: TickerInfo) -> f64 {
    Price::from_units(
        y_unit
            .0
            .saturating_mul(Price::min_tick_units(ticker_info.min_ticksize)),
    )
    .to_f64()
}

pub fn y_unit_of(level: f64, ticker_info: TickerInfo) -> YUnit {
    let unit = Price::min_tick_units(ticker_info.min_ticksize).max(1);
    YUnit(
        Price::from_f64(level)
            .round_to_min_tick(ticker_info.min_ticksize)
            .units
            / unit,
    )
}

fn price_level(alert: &StoredAlert) -> Option<f64> {
    match alert.kind {
        AlertKind::Price { level, .. } => Some(level),
        AlertKind::Indicator { .. } => None,
    }
}

impl AlertSync {
    /// Brings the lines and the store in step. Returns the store edits to
    /// apply and whether the chart needs a redraw.
    pub fn sync(
        &mut self,
        tools: &mut DrawingTools,
        store: &AlertStore,
        ticker_info: TickerInfo,
        basis: Basis,
        now: UnixMs,
        style: DrawingStyle,
    ) -> (Vec<StoreEdit>, bool) {
        if self.ticker != Some(ticker_info.ticker) {
            self.ticker = Some(ticker_info.ticker);
            self.synced.clear();
            self.deleted.clear();
        }
        self.deleted.retain(|id| store.get(id).is_some());
        let timeframe = match basis {
            Basis::Time(timeframe) => timeframe,
            Basis::Tick(_) => Timeframe::M1,
        };
        let new_alert = |id: String, y_unit: YUnit, armed: bool| StoredAlert {
            id,
            ticker: ticker_info.ticker,
            timeframe,
            kind: AlertKind::Price {
                cross: PriceCross::Cross,
                level: level_of(y_unit, ticker_info),
            },
            message: String::new(),
            frequency: Frequency::Once,
            channel: Channel::Desktop,
            enabled: armed,
            created_by: Creator::App,
            created_at: now.as_u64(),
            fired_at: None,
            error: None,
        };

        let mut edits = Vec::new();
        let mut redraw = false;
        let mut seen: HashSet<String> = HashSet::new();
        for line in tools.alert_lines() {
            let id = match line.id {
                // A copy of a line would share the ID: give it its own alert.
                Some(id) if !seen.contains(&id) => id,
                _ => {
                    let id = self.new_id(store, &seen);
                    tools.set_alert_id(line.drawing, id.clone());
                    edits.push(StoreEdit::Add(new_alert(
                        id.clone(),
                        line.y_unit,
                        line.armed,
                    )));
                    self.synced.insert(id.clone(), (line.y_unit, line.armed));
                    seen.insert(id);
                    continue;
                }
            };
            match store.get(&id) {
                Some(saved)
                    if saved.ticker == ticker_info.ticker && price_level(saved).is_some() =>
                {
                    let saved_y = y_unit_of(price_level(saved).unwrap_or_default(), ticker_info);
                    let (agreed_y, agreed_armed) = self
                        .synced
                        .get(&id)
                        .copied()
                        .unwrap_or((saved_y, saved.enabled));
                    let (mut y_unit, mut armed) = (line.y_unit, line.armed);
                    let mut level = None;
                    let mut enabled = None;
                    if line.y_unit != agreed_y {
                        level = Some(level_of(line.y_unit, ticker_info));
                    } else if saved_y != agreed_y {
                        redraw |= tools.update_alert_line(line.drawing, Some(saved_y), None);
                        y_unit = saved_y;
                    }
                    if line.armed != agreed_armed {
                        enabled = Some(line.armed);
                    } else if saved.enabled != agreed_armed {
                        redraw |= tools.update_alert_line(line.drawing, None, Some(saved.enabled));
                        armed = saved.enabled;
                    }
                    if level.is_some() || enabled.is_some() {
                        edits.push(StoreEdit::Update {
                            id: id.clone(),
                            level,
                            enabled,
                        });
                    }
                    self.synced.insert(id.clone(), (y_unit, armed));
                    seen.insert(id);
                }
                // Undo brought back a line deleted here: save it again.
                None if self.deleted.remove(&id) => {
                    edits.push(StoreEdit::Add(new_alert(
                        id.clone(),
                        line.y_unit,
                        line.armed,
                    )));
                    self.synced.insert(id.clone(), (line.y_unit, line.armed));
                    seen.insert(id);
                }
                // Deleted elsewhere, or not a price alert of this market.
                _ => {
                    redraw |= tools.remove_drawing_any(line.drawing);
                    self.synced.remove(&id);
                }
            }
        }

        // Lines deleted on this chart.
        let gone: Vec<String> = self
            .synced
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.synced.remove(&id);
            if store.get(&id).is_some() {
                edits.push(StoreEdit::Delete(id.clone()));
                self.deleted.insert(id);
            }
        }

        // Saved price alerts of this market with no line here yet.
        for saved in &store.alerts {
            let Some(level) = price_level(saved) else {
                continue;
            };
            if saved.ticker != ticker_info.ticker
                || seen.contains(&saved.id)
                || self.deleted.contains(&saved.id)
            {
                continue;
            }
            let y_unit = y_unit_of(level, ticker_info);
            tools.add_alert_line(saved.id.clone(), y_unit, saved.enabled, now, style);
            self.synced
                .insert(saved.id.clone(), (y_unit, saved.enabled));
            redraw = true;
        }
        (edits, redraw)
    }

    fn new_id(&self, store: &AlertStore, seen: &HashSet<String>) -> String {
        loop {
            let id = store.new_id();
            if !seen.contains(&id) && !self.synced.contains_key(&id) && !self.deleted.contains(&id)
            {
                return id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::adapter::Exchange;

    fn info() -> TickerInfo {
        TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        )
    }

    fn style() -> DrawingStyle {
        data::chart::kline::DrawingSettings::default()
            .style_defaults
            .style_for(data::chart::kline::DrawingTool::HorizontalLine)
    }

    fn run(
        sync: &mut AlertSync,
        tools: &mut DrawingTools,
        store: &mut AlertStore,
    ) -> Vec<StoreEdit> {
        let (edits, _) = sync.sync(
            tools,
            store,
            info(),
            Basis::Time(Timeframe::H1),
            UnixMs(1_000),
            style(),
        );
        // Apply like the app does.
        for edit in &edits {
            match edit {
                StoreEdit::Add(alert) => store.add(alert.clone()).unwrap(),
                StoreEdit::Update { id, level, enabled } => {
                    let alert = store.get_mut(id).unwrap();
                    if let (Some(new), AlertKind::Price { level, .. }) = (level, &mut alert.kind) {
                        *level = *new;
                    }
                    if let Some(enabled) = enabled {
                        alert.enabled = *enabled;
                    }
                }
                StoreEdit::Delete(id) => {
                    store.remove(id);
                }
            }
        }
        edits
    }

    fn saved_price(store: &AlertStore) -> Vec<(f64, bool)> {
        store
            .alerts
            .iter()
            .map(|alert| (price_level(alert).unwrap(), alert.enabled))
            .collect()
    }

    #[test]
    fn levels_round_trip_through_ticks() {
        let y = y_unit_of(62_000.0, info());
        assert_eq!(y, YUnit(620_000));
        assert_eq!(level_of(y, info()), 62_000.0);
    }

    #[test]
    fn a_saved_alert_gets_a_line_and_follows_the_store() {
        let mut tools = DrawingTools::default();
        let mut sync = AlertSync::default();
        let mut store = AlertStore::default();
        store
            .add(StoredAlert {
                id: "claude1".to_owned(),
                ticker: info().ticker,
                timeframe: Timeframe::H1,
                kind: AlertKind::Price {
                    cross: PriceCross::Above,
                    level: 62_000.0,
                },
                message: String::new(),
                frequency: Frequency::Once,
                channel: Channel::Both,
                enabled: true,
                created_by: Creator::Claude,
                created_at: 0,
                fired_at: None,
                error: None,
            })
            .unwrap();
        assert!(run(&mut sync, &mut tools, &mut store).is_empty());
        let lines = tools.alert_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].y_unit, YUnit(620_000));
        assert_eq!(lines[0].id.as_deref(), Some("claude1"));

        // The engine fired it (once): the line shows it disarmed.
        store.get_mut("claude1").unwrap().enabled = false;
        assert!(run(&mut sync, &mut tools, &mut store).is_empty());
        assert!(!tools.alert_lines()[0].armed);

        // Moved by Claude.
        if let AlertKind::Price { level, .. } = &mut store.get_mut("claude1").unwrap().kind {
            *level = 63_000.0;
        }
        run(&mut sync, &mut tools, &mut store);
        assert_eq!(tools.alert_lines()[0].y_unit, YUnit(630_000));

        // Deleted elsewhere: the line goes.
        store.remove("claude1");
        run(&mut sync, &mut tools, &mut store);
        assert!(tools.alert_lines().is_empty());
    }

    #[test]
    fn chart_lines_are_saved_moved_and_deleted() {
        let mut tools = DrawingTools::default();
        let mut sync = AlertSync::default();
        let mut store = AlertStore::default();
        // An alert line from an older layout: no ID yet.
        let line = tools.add_alert_line(String::new(), YUnit(600_000), true, UnixMs(0), style());
        tools.clear_alert_id_for_test(line);
        let edits = run(&mut sync, &mut tools, &mut store);
        assert!(matches!(&edits[..], [StoreEdit::Add(alert)]
            if alert.created_by == Creator::App && alert.frequency == Frequency::Once));
        assert_eq!(saved_price(&store), [(60_000.0, true)]);
        assert_eq!(
            store.alerts[0].kind,
            AlertKind::Price {
                cross: PriceCross::Cross,
                level: 60_000.0
            }
        );

        // Dragged on the chart and disarmed.
        tools.update_alert_line(line, Some(YUnit(610_000)), Some(false));
        run(&mut sync, &mut tools, &mut store);
        assert_eq!(saved_price(&store), [(61_000.0, false)]);

        // Deleted on the chart: the alert goes, and it is not re-added
        // while the store snapshot still lists it.
        tools.remove_drawing_any(line);
        let snapshot = store.clone();
        let edits = run(&mut sync, &mut tools, &mut store);
        assert!(matches!(&edits[..], [StoreEdit::Delete(_)]));
        let mut stale = snapshot;
        assert!(
            run(&mut sync, &mut tools, &mut stale)
                .iter()
                .all(|edit| !matches!(edit, StoreEdit::Add(_)))
        );
        assert!(tools.alert_lines().is_empty());
        assert!(store.alerts.is_empty());
    }

    #[test]
    fn other_markets_are_left_alone() {
        let mut tools = DrawingTools::default();
        let mut sync = AlertSync::default();
        let mut store = AlertStore::default();
        store
            .add(StoredAlert {
                id: "eth".to_owned(),
                ticker: Ticker::new("ETHUSDT", Exchange::BinanceLinear),
                timeframe: Timeframe::H1,
                kind: AlertKind::Price {
                    cross: PriceCross::Below,
                    level: 3_000.0,
                },
                message: String::new(),
                frequency: Frequency::Once,
                channel: Channel::Desktop,
                enabled: true,
                created_by: Creator::Claude,
                created_at: 0,
                fired_at: None,
                error: None,
            })
            .unwrap();
        assert!(run(&mut sync, &mut tools, &mut store).is_empty());
        assert!(tools.alert_lines().is_empty());
        assert_eq!(store.alerts.len(), 1);
    }
}
