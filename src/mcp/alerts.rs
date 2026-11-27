//! MCP tools that manage the saved alerts (`alerts.json`): create, list,
//! update, delete, pause and resume. The Orbit Tape app or `orbit-tape
//! watch` runs them; these tools only write the file. Still no orders, keys
//! or trading actions.

use super::{OrbitTapeMcp, inputs, market_id, symbol_coin, timeframe, to_text, utc, venue_id};
use crate::headless::PerpExchange;
use crate::headless::{alert_plots, indicator_config};
use data::config::alert_store::{
    AlertKind, AlertStore, Channel, Creator, Frequency, MAX_ACTIVE, PriceCross, StoredAlert,
    trim_float,
};
use data::config::shared_file;
use exchange::Timeframe;
use rmcp::{handler::server::wrapper::Parameters, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAlertParams {
    /// Coin or instrument, e.g. BTC, ETHUSDT, BTC-USDT-SWAP.
    pub symbol: String,
    /// binance or okx. Default: the exchange line of the user's
    /// scan-symbols.txt, else okx (always okx for spot).
    pub exchange: Option<String>,
    /// perp (the USDT perpetual) or spot (the USDT spot pair, OKX only, e.g.
    /// BTC-USDT). Default perp.
    pub market: Option<String>,
    /// crosses_above, crosses_below or crosses (either way) a price, or
    /// indicator for an indicator's alert condition on a closed bar.
    pub condition: String,
    /// The price level, for the crosses conditions.
    pub price: Option<f64>,
    /// For indicator: installed indicator name, ID or title, e.g.
    /// roman-setup.
    pub indicator: Option<String>,
    /// For indicator: one alert condition, by title or plot ID (see
    /// list_indicators). Default: any of the indicator's alert conditions.
    pub alert: Option<String>,
    /// For indicator: inputs by id or label, values as text, e.g.
    /// {"ctMode": "true"}.
    pub inputs: Option<BTreeMap<String, String>>,
    /// 1m 3m 5m 15m 30m 1h 2h 4h 12h 1d 1w. Default 1h. Indicator alerts run on
    /// its closed bars; every_bar price alerts fire at most once per bar of it.
    pub timeframe: Option<String>,
    /// once (fire, then turn off) or every_bar. Default: once for price
    /// alerts, every_bar for indicator alerts.
    pub frequency: Option<String>,
    /// desktop, ntfy (the user's phone) or both. Default desktop.
    pub channel: Option<String>,
    /// Extra text for the notification.
    pub message: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateAlertParams {
    /// The alert ID (from create_alert or list_alerts).
    pub id: String,
    /// New price level (price alerts).
    pub price: Option<f64>,
    /// crosses_above, crosses_below or crosses (price alerts).
    pub condition: Option<String>,
    /// once or every_bar.
    pub frequency: Option<String>,
    /// desktop, ntfy or both.
    pub channel: Option<String>,
    /// Notification text; an empty string removes it.
    pub message: Option<String>,
    /// false pauses the alert, true resumes it.
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AlertIdParams {
    /// The alert ID (from create_alert or list_alerts).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AlertIdsParams {
    /// Alert IDs. Omit for all alerts.
    pub ids: Option<Vec<String>>,
}

fn parse_cross(text: &str) -> Option<PriceCross> {
    match text.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
        "crosses_above" | "above" => Some(PriceCross::Above),
        "crosses_below" | "below" => Some(PriceCross::Below),
        "crosses" | "cross" => Some(PriceCross::Cross),
        _ => None,
    }
}

fn parse_frequency(text: &str) -> Result<Frequency, String> {
    match text.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
        "once" => Ok(Frequency::Once),
        "every_bar" => Ok(Frequency::EveryBar),
        _ => Err(format!("unknown frequency '{text}' (once or every_bar)")),
    }
}

fn parse_channel(text: &str) -> Result<Channel, String> {
    match text.trim().to_lowercase().as_str() {
        "desktop" => Ok(Channel::Desktop),
        "ntfy" | "phone" => Ok(Channel::Ntfy),
        "both" => Ok(Channel::Both),
        _ => Err(format!("unknown channel '{text}' (desktop, ntfy or both)")),
    }
}

/// Which market an alert watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Market {
    Perp,
    Spot,
}

fn parse_market(text: &str) -> Result<Market, String> {
    match text.trim().to_lowercase().as_str() {
        "perp" | "perps" | "perpetual" | "swap" => Ok(Market::Perp),
        "spot" => Ok(Market::Spot),
        _ => Err(format!("unknown market '{text}' (perp or spot)")),
    }
}

fn valid_price(price: f64) -> Result<f64, String> {
    if price.is_finite() && price > 0.0 {
        Ok(price)
    } else {
        Err(format!("price must be a positive number, got {price}"))
    }
}

fn cross_id(cross: PriceCross) -> &'static str {
    match cross {
        PriceCross::Above => "crosses_above",
        PriceCross::Below => "crosses_below",
        PriceCross::Cross => "crosses",
    }
}

/// One alert as the tools return it.
pub fn alert_json(alert: &StoredAlert) -> Json {
    let mut out = json!({
        "id": alert.id,
        "summary": alert.summary(),
        "symbol": alert.symbol(),
        "exchange": venue_id(alert.ticker.exchange.venue()),
        "market": market_id(alert.ticker.market_type()),
        "timeframe": alert.timeframe.to_string(),
        "frequency": match alert.frequency {
            Frequency::Once => "once",
            Frequency::EveryBar => "every_bar",
        },
        "channel": match alert.channel {
            Channel::Desktop => "desktop",
            Channel::Ntfy => "ntfy",
            Channel::Both => "both",
        },
        "status": if alert.is_done() {
            "fired"
        } else if alert.enabled {
            "active"
        } else {
            "paused"
        },
        "created_by": match alert.created_by {
            Creator::App => "app",
            Creator::Claude => "claude",
        },
        "created_utc": utc(alert.created_at as i64),
        "fired_utc": alert.fired_at.map(|time| utc(time as i64)),
    });
    match &alert.kind {
        AlertKind::Price { cross, level } => {
            out["condition"] = json!(cross_id(*cross));
            out["price"] = json!(level);
        }
        AlertKind::Indicator {
            indicator,
            plot,
            inputs,
        } => {
            out["condition"] = json!("indicator");
            out["indicator"] = json!(indicator);
            out["alert"] = json!(plot);
            if !inputs.is_empty() {
                out["inputs"] = json!(inputs);
            }
        }
    }
    if !alert.message.is_empty() {
        out["message"] = json!(alert.message);
    }
    if let Some(error) = &alert.error {
        out["error"] = json!(error);
    }
    out
}

impl OrbitTapeMcp {
    /// Whether a program runs the alerts now, for the tools to tell.
    fn engine_status(&self) -> &'static str {
        match shared_file::try_lock(&self.paths.engine_lock) {
            // Nobody held it; the lock is released again at once.
            Ok(Some(_)) => {
                "not running: alerts fire while the Orbit Tape app is open or orbit-tape watch runs (systemctl --user enable --now orbit-tape-watch)"
            }
            Ok(None) => "running",
            Err(_) => "unknown",
        }
    }

    fn read_store(&self) -> Result<AlertStore, String> {
        shared_file::read(&self.paths.alerts).map_err(|error| error.to_string())
    }

    fn change_store<R>(
        &self,
        change: impl FnOnce(&mut AlertStore) -> Result<R, String>,
    ) -> Result<R, String> {
        AlertStore::update_in(&self.paths.alerts, change)
            .map_err(|error| format!("could not save the alerts: {error}"))?
    }

    fn check_ntfy(&self, channel: Channel) -> Result<(), String> {
        if channel.ntfy() && !self.read_store()?.channels.ntfy.is_set() {
            return Err("no ntfy topic is set; the user sets it in Orbit Tape: Settings, Alerts, Phone (ntfy). Use channel desktop until then".to_owned());
        }
        Ok(())
    }
}

#[tool_router(router = alert_tool_router, vis = "pub(crate)")]
impl OrbitTapeMcp {
    #[tool(
        description = "Create a saved alert that Orbit Tape runs (also while the app is closed, when the user runs orbit-tape watch): a price crossing a level on live trades, or an indicator's alert condition on a closed bar. On a USDT perpetual (default) or, with market spot, an OKX USDT spot pair such as BTC-USDT. Returns the alert ID and a one-line summary. At most 50 active alerts. Only writes the local alerts file; places no orders."
    )]
    pub async fn create_alert(
        &self,
        Parameters(params): Parameters<CreateAlertParams>,
    ) -> Result<String, String> {
        let market = params
            .market
            .as_deref()
            .map_or(Ok(Market::Perp), parse_market)?;
        let exchange = match market {
            Market::Perp => self.exchange(params.exchange.as_deref())?,
            Market::Spot => match params.exchange.as_deref() {
                None => PerpExchange::Okx,
                Some(text) => match super::perp_exchange(text)? {
                    PerpExchange::Okx => PerpExchange::Okx,
                    other => {
                        return Err(format!(
                            "spot alerts are OKX only, not {}; use exchange okx",
                            other.name()
                        ));
                    }
                },
            },
        };
        let coin = symbol_coin(&params.symbol)?;
        let timeframe = timeframe(params.timeframe.as_deref())?;
        let channel = params
            .channel
            .as_deref()
            .map_or(Ok(Channel::Desktop), parse_channel)?;
        self.check_ntfy(channel)?;

        let (kind, default_frequency) = if params.condition.trim().eq_ignore_ascii_case("indicator")
        {
            let (_, indicator) = self.indicator(Some(
                params
                    .indicator
                    .as_deref()
                    .ok_or("condition indicator needs an indicator")?,
            ))?;
            let plots = alert_plots(&indicator);
            if plots.is_empty() {
                return Err(format!(
                    "{} has no alert conditions",
                    indicator.descriptor.title
                ));
            }
            let plot = match params.alert.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(wanted) => Some(
                    plots
                        .iter()
                        .find(|(id, title)| id == wanted || title.eq_ignore_ascii_case(wanted))
                        .map(|(id, _)| id.clone())
                        .ok_or_else(|| {
                            format!(
                                "{} has no alert condition '{wanted}' (it has: {})",
                                indicator.descriptor.title,
                                plots
                                    .iter()
                                    .map(|(_, title)| title.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        })?,
                ),
            };
            let inputs: Vec<(String, String)> = inputs(params.inputs);
            indicator_config(&indicator, &inputs)?;
            // Saved by input ID, so a renamed label still works.
            let inputs = inputs
                .into_iter()
                .map(|(key, value)| {
                    let id = indicator
                        .descriptor
                        .inputs
                        .iter()
                        .find(|input| input.id == key || input.label == key)
                        .map_or(key, |input| input.id.clone());
                    (id, value)
                })
                .collect();
            (
                AlertKind::Indicator {
                    indicator: indicator.descriptor.id.as_str().to_owned(),
                    plot,
                    inputs,
                },
                Frequency::EveryBar,
            )
        } else {
            let cross = parse_cross(&params.condition).ok_or_else(|| {
                format!(
                    "unknown condition '{}' (crosses_above, crosses_below, crosses or indicator)",
                    params.condition
                )
            })?;
            let level = valid_price(params.price.ok_or("price alerts need a price")?)?;
            (AlertKind::Price { cross, level }, Frequency::Once)
        };
        let frequency = params
            .frequency
            .as_deref()
            .map_or(Ok(default_frequency), parse_frequency)?;

        let (ticker_info, last_close) = match market {
            Market::Perp => {
                let (_, ticker_info) = self.candles.resolve(exchange, coin.clone()).await?;
                let last_close = self
                    .candles
                    .closed_candles(exchange, coin, Timeframe::M1, 1)
                    .await
                    .ok()
                    .and_then(|series| series.klines.last().map(|kline| kline.close.to_f64()));
                (ticker_info, last_close)
            }
            // The pair's details come with its candles; an hour of them, so
            // a quiet pair still has one.
            Market::Spot => {
                let series = self
                    .candles
                    .spot_candles(format!("{coin}-USDT"), Timeframe::M1, 60)
                    .await?;
                let last_close = series.klines.last().map(|kline| kline.close.to_f64());
                (series.ticker_info, last_close)
            }
        };

        let alert = self.change_store(|store| {
            let alert = StoredAlert {
                id: store.new_id(),
                ticker: ticker_info.ticker,
                timeframe,
                kind,
                message: params.message.unwrap_or_default().trim().to_owned(),
                frequency,
                channel,
                enabled: true,
                created_by: Creator::Claude,
                created_at: exchange::UnixMs::now().as_u64(),
                fired_at: None,
                error: None,
            };
            store.add(alert.clone())?;
            Ok(alert)
        })?;

        let mut out = json!({
            "id": alert.id,
            "summary": alert.summary(),
            "alert": alert_json(&alert),
            "engine": self.engine_status(),
        });
        if let (Some(close), AlertKind::Price { cross, level }) = (last_close, &alert.kind) {
            out["last_close"] = json!(close);
            let note = match cross {
                PriceCross::Above if close >= *level => Some("above"),
                PriceCross::Below if close <= *level => Some("below"),
                _ => None,
            };
            if let Some(side) = note {
                out["note"] = json!(format!(
                    "price is already {side} {}; the alert fires when it comes back and crosses again",
                    trim_float(*level)
                ));
            }
        }
        to_text(&out)
    }

    #[tool(
        description = "List the saved alerts (made in the app or by Claude) with ID, summary, status (active, paused, fired), frequency, channel and any error, and whether a program runs them now."
    )]
    pub async fn list_alerts(&self) -> Result<String, String> {
        let store = self.read_store()?;
        to_text(&json!({
            "alerts": store.alerts.iter().map(alert_json).collect::<Vec<_>>(),
            "active": store.active_count(),
            "max_active": MAX_ACTIVE,
            "engine": self.engine_status(),
        }))
    }

    #[tool(
        description = "Change a saved alert: price level and direction (price alerts), frequency, channel, message, or enabled (false pauses, true resumes). Returns the new summary."
    )]
    pub async fn update_alert(
        &self,
        Parameters(params): Parameters<UpdateAlertParams>,
    ) -> Result<String, String> {
        let frequency = params
            .frequency
            .as_deref()
            .map(parse_frequency)
            .transpose()?;
        let channel = params.channel.as_deref().map(parse_channel).transpose()?;
        if let Some(channel) = channel {
            self.check_ntfy(channel)?;
        }
        let cross = params
            .condition
            .as_deref()
            .map(|text| {
                parse_cross(text).ok_or_else(|| {
                    format!("unknown condition '{text}' (crosses_above, crosses_below or crosses)")
                })
            })
            .transpose()?;
        let price = params.price.map(valid_price).transpose()?;
        let alert = self.change_store(|store| {
            if let Some(enabled) = params.enabled {
                store.set_enabled(&params.id, enabled)?;
            }
            let alert = store
                .get_mut(&params.id)
                .ok_or_else(|| format!("no alert with ID {}", params.id))?;
            match &mut alert.kind {
                AlertKind::Price {
                    cross: saved_cross,
                    level,
                } => {
                    if let Some(cross) = cross {
                        *saved_cross = cross;
                    }
                    if let Some(price) = price {
                        *level = price;
                    }
                }
                AlertKind::Indicator { .. } if cross.is_some() || price.is_some() => {
                    return Err("price and condition only apply to price alerts; delete and create an indicator alert to change it".to_owned());
                }
                AlertKind::Indicator { .. } => {}
            }
            if let Some(frequency) = frequency {
                alert.frequency = frequency;
            }
            if let Some(channel) = channel {
                alert.channel = channel;
            }
            if let Some(message) = &params.message {
                alert.message = message.trim().to_owned();
            }
            Ok(alert.clone())
        })?;
        to_text(&json!({ "id": alert.id, "summary": alert.summary(), "alert": alert_json(&alert) }))
    }

    #[tool(description = "Delete a saved alert by ID. Returns what was deleted.")]
    pub async fn delete_alert(
        &self,
        Parameters(params): Parameters<AlertIdParams>,
    ) -> Result<String, String> {
        let alert = self.change_store(|store| {
            store
                .remove(&params.id)
                .ok_or_else(|| format!("no alert with ID {}", params.id))
        })?;
        to_text(&json!({ "deleted": alert.id, "summary": alert.summary() }))
    }

    #[tool(
        description = "Pause saved alerts (they stay saved but do not fire). Pass IDs, or nothing to pause all."
    )]
    pub async fn pause_alerts(
        &self,
        Parameters(params): Parameters<AlertIdsParams>,
    ) -> Result<String, String> {
        self.set_many(params.ids, false)
    }

    #[tool(
        description = "Resume paused alerts. Pass IDs (this also re-arms once alerts that already fired), or nothing to resume every paused alert that has not fired. At most 50 active."
    )]
    pub async fn resume_alerts(
        &self,
        Parameters(params): Parameters<AlertIdsParams>,
    ) -> Result<String, String> {
        self.set_many(params.ids, true)
    }
}

impl OrbitTapeMcp {
    fn set_many(&self, ids: Option<Vec<String>>, enabled: bool) -> Result<String, String> {
        let (changed, active) = self.change_store(|store| {
            let ids: Vec<String> = match ids {
                Some(ids) => {
                    if let Some(missing) = ids.iter().find(|id| store.get(id).is_none()) {
                        return Err(format!("no alert with ID {missing}"));
                    }
                    ids
                }
                None => store
                    .alerts
                    .iter()
                    .filter(|alert| alert.enabled != enabled && !alert.is_done())
                    .map(|alert| alert.id.clone())
                    .collect(),
            };
            let mut changed = Vec::new();
            for id in ids {
                if store.get(&id).is_some_and(|alert| alert.enabled == enabled) {
                    continue;
                }
                store.set_enabled(&id, enabled)?;
                changed.push(id);
            }
            Ok((changed, store.active_count()))
        })?;
        let mut out = json!({ "active": active });
        out[if enabled { "resumed" } else { "paused" }] = json!(changed);
        to_text(&out)
    }
}
