//! MCP tools for the daily routine: the user's plan, a market snapshot of
//! the plan's universe, the Roman setups and the spot status built on it.

mod math;
pub(crate) mod setups;
pub(crate) mod spot;

use super::{
    DEFAULT_RUN_BARS, OrbitTapeMcp, Precision, Series, coin, exchange_id, indicator_config, number,
    perp_exchange, round_decimals, to_text, utc,
};
use crate::headless::{self, PerpExchange};
use data::config::plan::Plan;
use exchange::Timeframe;
use indicator_engine::CompiledIndicator;
use rmcp::{handler::server::wrapper::Parameters, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The Roman Setup indicator the routine runs.
pub(crate) const ROMAN: &str = "roman.roman-setup";
/// Coins worked on at once (each fetches several timeframes; the candle
/// source caps requests per exchange).
const COINS_PARALLEL: usize = 4;
const RSI_LENGTH: usize = 14;
const ATR_LENGTH: usize = 14;
const SMA_LENGTH: usize = 200;
const DAILY_BARS: usize = 260;
const H4_BARS: usize = 300;
const WEEKLY_BARS: usize = 260;
const HOURS_PER_DAY: usize = 24;
const HOURS_PER_WEEK: usize = 168;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpotParams {
    /// The day to check, YYYY-MM-DD. Default: today (local time).
    pub date: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetupsParams {
    /// Coins, e.g. ["BTC", "ETH"]. Default: the plan's universe.
    pub coins: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapshotParams {
    /// Coins, e.g. ["BTC", "ETH"]. Default: the plan's universe.
    pub coins: Option<Vec<String>>,
    /// binance or okx. Default: the plan's exchange.
    pub exchange: Option<String>,
}

impl OrbitTapeMcp {
    pub(crate) fn plan(&self) -> Result<Plan, String> {
        Plan::load_from(&self.paths.plan)
    }

    /// The coins and exchange to work on: the parameters, else the plan.
    pub(crate) fn universe(
        &self,
        coins: Option<Vec<String>>,
        exchange: Option<&str>,
        plan: Option<&Plan>,
    ) -> Result<(PerpExchange, Vec<String>), String> {
        let exchange = match (exchange, plan) {
            (Some(text), _) => perp_exchange(text)?,
            (None, Some(plan)) => perp_exchange(&plan.universe.exchange)?,
            (None, None) => PerpExchange::Okx,
        };
        let coins = match (coins, plan) {
            (Some(coins), _) => coins,
            (None, Some(plan)) => plan.universe.coins.clone(),
            (None, None) => return Err("no coins given and no plan to take them from".to_owned()),
        };
        let mut unique: Vec<String> = Vec::new();
        for symbol in coins {
            let coin = coin(&symbol);
            if !coin.is_empty() && !unique.contains(&coin) {
                unique.push(coin);
            }
        }
        if unique.is_empty() {
            return Err("no coins to look at".to_owned());
        }
        Ok((exchange, unique))
    }

    /// Roman Setup on a coin's closed 1h candles with the plan's inputs for
    /// it: every x_* setup value of the newest closed bar by title.
    pub(crate) fn roman_values(
        &self,
        indicator: &CompiledIndicator,
        coin: &str,
        plan: Option<&Plan>,
        series: &Series,
    ) -> Result<BTreeMap<String, Option<f64>>, String> {
        let inputs: Vec<(String, String)> = plan
            .map(|plan| plan.roman.inputs_for(coin).into_iter().collect())
            .unwrap_or_default();
        let config = indicator_config(indicator, &inputs)?;
        let mut last = None;
        headless::run_closed(
            indicator,
            &config,
            series.ticker_info,
            &series.klines,
            |_, output| last = Some(output.clone()),
        )?;
        let output = last.ok_or("no candles")?;
        Ok(output
            .plots
            .iter()
            .filter(|plot| plot.id.starts_with("x_"))
            .map(|plot| (plot.id.clone(), number(&plot.value)))
            .collect())
    }

    async fn coin_snapshot(
        &self,
        exchange: PerpExchange,
        coin: String,
        roman: &CompiledIndicator,
        plan: Option<&Plan>,
    ) -> Result<Json, String> {
        let candles = &self.candles;
        let (hours, days, h4, weeks) = iced::futures::join!(
            candles.closed_candles(exchange, coin.clone(), Timeframe::H1, DEFAULT_RUN_BARS),
            candles.closed_candles(exchange, coin.clone(), Timeframe::D1, DAILY_BARS),
            candles.closed_candles(exchange, coin.clone(), Timeframe::H4, H4_BARS),
            candles.closed_candles(exchange, coin.clone(), Timeframe::W1, WEEKLY_BARS),
        );
        let hours = hours?;
        let days = days?;
        let roman = self.roman_values(roman, &coin, plan, &hours)?;
        let snapshot = Snapshot::new(&hours, &days, h4.ok().as_ref(), weeks.ok().as_ref());
        Ok(snapshot.to_json(&coin, &hours, roman))
    }
}

impl OrbitTapeMcp {
    async fn coin_setup(
        &self,
        exchange: PerpExchange,
        coin: String,
        roman: &CompiledIndicator,
        plan: &Plan,
    ) -> Result<Json, String> {
        let candles = &self.candles;
        let (hours, days) = iced::futures::join!(
            candles.closed_candles(exchange, coin.clone(), Timeframe::H1, DEFAULT_RUN_BARS),
            candles.closed_candles(exchange, coin.clone(), Timeframe::D1, DAILY_BARS),
        );
        let (hours, days) = (hours?, days?);
        let values = self.roman_values(roman, &coin, Some(plan), &hours)?;
        let snapshot = Snapshot::new(&hours, &days, None, None);
        let input = setups::CoinInput {
            close: snapshot.close,
            atr: snapshot.atr_1d,
            high_24h: snapshot.high_24h,
            low_24h: snapshot.low_24h,
            high_1m: snapshot.high_1m,
            low_1m: snapshot.low_1m,
            values,
        };
        let rules = setups::Rules {
            max_stop_pct: plan.prop.max_stop_pct,
            leverage: plan.prop.leverage_of(&coin),
            risk_pct: plan.prop.risk_pct,
            counter_trend_risk_pct: plan.prop.counter_trend_risk_pct,
        };
        let setup = input.setup(&rules);
        let mut out = serde_json::to_value(&setup).map_err(|error| error.to_string())?;
        rounded(&mut out, Precision::of(&hours.ticker_info));
        out["base"] = json!(coin);
        out["symbol"] = json!(hours.instrument);
        out["close"] = json!(Precision::of(&hours.ticker_info).price(snapshot.close));
        out["as_of_bar_utc"] = json!(hours.klines.last().map(|k| utc(k.time.as_u64() as i64)));
        out["atr_1d"] = json!(
            snapshot
                .atr_1d
                .map(|atr| Precision::of(&hours.ticker_info).price(atr))
        );
        Ok(out)
    }
}

/// Rounds a setup for reading: prices to the tick, percentages and R
/// multiples to 2 decimals, sizes to whole units.
fn rounded(value: &mut Json, precision: Precision) {
    match value {
        Json::Object(map) => {
            for (key, value) in map.iter_mut() {
                if let Some(number) = value.as_f64() {
                    let decimals = match key.as_str() {
                        "near" | "far" | "entry" | "stop" | "t1" | "t2" | "level" => None,
                        key if key.ends_with("_pct") || key.ends_with("_r") => Some(2),
                        "notional_per_10" | "margin_per_10" => Some(0),
                        _ => continue,
                    };
                    *value = json!(match decimals {
                        None => precision.price(number),
                        Some(decimals) => round_decimals(number, decimals),
                    });
                } else {
                    rounded(value, precision);
                }
            }
        }
        Json::Array(items) => items.iter_mut().for_each(|item| rounded(item, precision)),
        _ => {}
    }
}

/// Rounds the spot status: prices to the tick, percentages and RSI to 2
/// decimals.
fn spot_rounded(value: &mut Json, precision: Precision) {
    match value {
        Json::Object(map) => {
            for (key, value) in map.iter_mut() {
                if let Some(number) = value.as_f64() {
                    let rounded = match key.as_str() {
                        "close" | "price" | "reference_price" | "sma200_1w" => {
                            precision.price(number)
                        }
                        key if key.ends_with("_pct") || key.starts_with("rsi_") => {
                            round_decimals(number, 2)
                        }
                        _ => continue,
                    };
                    *value = json!(rounded);
                } else {
                    spot_rounded(value, precision);
                }
            }
        }
        Json::Array(items) => items
            .iter_mut()
            .for_each(|item| spot_rounded(item, precision)),
        _ => {}
    }
}

/// One coin's numbers, from its closed candles.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    pub close: f64,
    pub day_pct: Option<f64>,
    pub week_pct: Option<f64>,
    pub high_24h: Option<f64>,
    pub low_24h: Option<f64>,
    pub rsi_1d: Option<f64>,
    pub rsi_4h: Option<f64>,
    pub rsi_1w: Option<f64>,
    pub atr_1d: Option<f64>,
    pub high_1m: Option<f64>,
    pub low_1m: Option<f64>,
    pub low_3m: Option<f64>,
    pub sma200_1d: Option<f64>,
    pub sma200_1w: Option<f64>,
    /// Values missing for lack of history, and why.
    pub notes: Vec<String>,
}

fn closes(series: &Series) -> Vec<f64> {
    series.klines.iter().map(|k| k.close.to_f64()).collect()
}

impl Snapshot {
    /// `hours` and `days` are needed; 4h and weekly values are left empty
    /// when those could not be fetched.
    pub fn new(hours: &Series, days: &Series, h4: Option<&Series>, weeks: Option<&Series>) -> Self {
        let mut notes = Vec::new();
        let hour_closes = closes(hours);
        let close = hour_closes.last().copied().unwrap_or_default();
        let ago = |bars: usize| {
            hour_closes
                .len()
                .checked_sub(bars + 1)
                .map(|index| hour_closes[index])
        };
        let last_day: Vec<_> = hours.klines.iter().rev().take(HOURS_PER_DAY).collect();
        let full_day = last_day.len() == HOURS_PER_DAY;
        // Highs and lows of the last `count` closed days plus the hours since
        // the newest of them closed.
        let day_ms = Timeframe::D1.to_milliseconds();
        let since_last_day = |count: usize| -> Option<(f64, f64)> {
            if days.klines.len() < count {
                return None;
            }
            let recent = &days.klines[days.klines.len() - count..];
            let after = recent.last().map(|k| k.time.as_u64() + day_ms)?;
            let bars = recent
                .iter()
                .map(|k| (k.high.to_f64(), k.low.to_f64()))
                .chain(
                    hours
                        .klines
                        .iter()
                        .filter(|k| k.time.as_u64() >= after)
                        .map(|k| (k.high.to_f64(), k.low.to_f64())),
                );
            Some(bars.fold((f64::MIN, f64::MAX), |(high, low), (h, l)| {
                (high.max(h), low.min(l))
            }))
        };
        let month = since_last_day(30);
        let quarter = since_last_day(90);
        let day_closes = closes(days);
        let day_bars: Vec<(f64, f64, f64)> = days
            .klines
            .iter()
            .map(|k| (k.high.to_f64(), k.low.to_f64(), k.close.to_f64()))
            .collect();
        let rsi_4h = h4.and_then(|series| math::rsi(&closes(series), RSI_LENGTH));
        let week_closes = weeks.map(closes).unwrap_or_default();
        let sma200_1w = math::sma(&week_closes, SMA_LENGTH);
        let sma200_1d = math::sma(&day_closes, SMA_LENGTH);
        if sma200_1d.is_none() {
            notes.push(format!("sma200_1d: only {} daily bars", day_closes.len()));
        }
        if sma200_1w.is_none() {
            notes.push(match weeks {
                Some(_) => format!("sma200_1w: only {} weekly bars", week_closes.len()),
                None => "sma200_1w, rsi_1w: weekly candles could not be fetched".to_owned(),
            });
        }
        if h4.is_none() {
            notes.push("rsi_4h: 4h candles could not be fetched".to_owned());
        }
        if month.is_none() {
            notes.push(format!(
                "high_1m, low_1m: only {} daily bars",
                days.klines.len()
            ));
        }
        Self {
            close,
            day_pct: ago(HOURS_PER_DAY).and_then(|from| math::pct(from, close)),
            week_pct: ago(HOURS_PER_WEEK).and_then(|from| math::pct(from, close)),
            high_24h: full_day.then(|| {
                last_day
                    .iter()
                    .map(|k| k.high.to_f64())
                    .fold(f64::MIN, f64::max)
            }),
            low_24h: full_day.then(|| {
                last_day
                    .iter()
                    .map(|k| k.low.to_f64())
                    .fold(f64::MAX, f64::min)
            }),
            rsi_1d: math::rsi(&day_closes, RSI_LENGTH),
            rsi_4h,
            rsi_1w: math::rsi(&week_closes, RSI_LENGTH),
            atr_1d: math::atr(&day_bars, ATR_LENGTH),
            high_1m: month.map(|(high, _)| high),
            low_1m: month.map(|(_, low)| low),
            low_3m: quarter.map(|(_, low)| low),
            sma200_1d,
            sma200_1w,
            notes,
        }
    }

    fn to_json(&self, coin: &str, hours: &Series, roman: BTreeMap<String, Option<f64>>) -> Json {
        let precision = Precision::of(&hours.ticker_info);
        let price = |value: Option<f64>| value.map(|value| precision.price(value));
        let two = |value: Option<f64>| value.map(|value| round_decimals(value, 2));
        let vs = |level: Option<f64>| {
            level
                .and_then(|level| math::pct(level, self.close))
                .map(|value| round_decimals(value, 2))
        };
        let bias = match roman.get("x_bias").copied().flatten() {
            Some(value) if value > 0.0 => "bullish",
            Some(value) if value < 0.0 => "bearish",
            _ => "none",
        };
        json!({
            "base": coin,
            "symbol": hours.instrument,
            "last_closed_hour_utc": hours.klines.last().map(|k| utc(k.time.as_u64() as i64)),
            "close": precision.price(self.close),
            "day_pct": two(self.day_pct),
            "week_pct": two(self.week_pct),
            "high_24h": price(self.high_24h),
            "low_24h": price(self.low_24h),
            "rsi_1d": two(self.rsi_1d),
            "rsi_4h": two(self.rsi_4h),
            "rsi_1w": two(self.rsi_1w),
            "atr_1d": price(self.atr_1d),
            "high_1m": price(self.high_1m),
            "low_1m": price(self.low_1m),
            "low_3m": price(self.low_3m),
            "sma200_1d": price(self.sma200_1d),
            "vs_sma200_1d_pct": vs(self.sma200_1d),
            "sma200_1w": price(self.sma200_1w),
            "vs_sma200_1w_pct": vs(self.sma200_1w),
            "roman_12h_bias": bias,
            "notes": self.notes,
        })
    }
}

#[tool_router(router = routine_tool_router, vis = "pub(crate)")]
impl OrbitTapeMcp {
    #[tool(
        description = "The user's trading plan (plan.toml): prop-firm rules (risk %, counter-trend risk %, max correlated positions, max stop %, leverage by coin), the coin universe and exchange, Roman Setup input overrides, the spot accumulation plan (holdings, goal, budget, reference price, heads-up and buy levels with amounts and which were used, monthly-buy dates and skipped dates) and report settings. Read it before market_snapshot, roman_setups or spot_status."
    )]
    pub async fn get_plan(&self) -> Result<String, String> {
        let plan = self.plan()?;
        to_text(&json!({
            "path": self.paths.plan.display().to_string(),
            "plan": plan,
        }))
    }

    #[tool(
        description = "Roman Setup (H12 bias, H1 execution) on every coin of the plan's universe (or the coins given) with the plan's inputs, and the routine's rules applied: the primary bias-aligned level (nearest resistance zone or broken level for a bearish bias, support for a bullish one, on the trade's side of price) and its distance %, entry (its near edge, or the broken level or 50% fib if nearer; the close when price is inside), stop beyond the area + 0.3 daily ATR, T1 (24h low/high) and T2 (1M low/high) with R multiples, notional and margin per $10 risked at the plan's max leverage. Statuses: LIVE (a tap on the bias side in the last 24 1h bars and a bias-aligned 1h MSS after it), WATCH (within 5% of the primary level), FLIP-WATCH (within 3% of the 12H bias-flip level), COUNTER-TREND (armed after an HTF SFP against the bias: entry after the next 1h MSS, stop beyond the SFP wick + 0.2 ATR, counter-trend risk). Setups whose stop is over the plan's max stop % are skipped with the reason. Script output, not advice."
    )]
    pub async fn roman_setups(
        &self,
        Parameters(params): Parameters<SetupsParams>,
    ) -> Result<String, String> {
        use iced::futures::StreamExt;
        let plan = self.plan()?;
        let (exchange, coins) = self.universe(params.coins, None, Some(&plan))?;
        let (_, roman) = self.indicator(Some(ROMAN))?;
        let rows: Vec<Json> = iced::futures::stream::iter(coins)
            .map(|coin| {
                let (roman, plan) = (&roman, &plan);
                async move {
                    self.coin_setup(exchange, coin.clone(), roman, plan)
                        .await
                        .unwrap_or_else(|error| json!({ "base": coin, "error": error }))
                }
            })
            .buffered(COINS_PARALLEL)
            .collect()
            .await;
        let group = |status: &str| -> Vec<Json> {
            rows.iter()
                .filter(|row| {
                    row["statuses"]
                        .as_array()
                        .is_some_and(|list| list.iter().any(|s| s == status))
                })
                .map(|row| row["base"].clone())
                .collect()
        };
        to_text(&json!({
            "exchange": exchange_id(exchange),
            "live": group("LIVE"),
            "watch": group("WATCH"),
            "flip_watch": group("FLIP-WATCH"),
            "counter_trend": group("COUNTER-TREND"),
            "failed": rows.iter().filter(|row| row.get("error").is_some()).count(),
            "rules": {
                "risk_pct": plan.prop.risk_pct,
                "counter_trend_risk_pct": plan.prop.counter_trend_risk_pct,
                "max_correlated": plan.prop.max_correlated,
                "max_stop_pct": plan.prop.max_stop_pct,
                "live_tap_bars": setups::LIVE_BARS,
                "watch_pct": setups::WATCH_PCT,
                "flip_watch_pct": setups::FLIP_PCT,
            },
            "coins": rows,
        }))
    }

    #[tool(
        description = "The plan's spot accumulation status on OKX spot (e.g. BTC-USDT), from closed candles: price, vs the plan's reference price (%), the next heads-up or unused buy level below price and its distance, RSI(14) 1D and 1W, vs SMA200 1W (%), monthly buys done (skipped dates do not count) and the next date, lump-sum parts with which are used and the amount left, and after the last monthly date the first-Monday buys that spread the unused parts. spot_zone is true on a monthly-buy day or when price is within the plan's zone % of (or below) an unused buy level; zone_reasons says why and what the plan says to do. Plan output, not advice."
    )]
    pub async fn spot_status(
        &self,
        Parameters(params): Parameters<SpotParams>,
    ) -> Result<String, String> {
        let plan = self.plan()?;
        let spot_plan = plan.spot.as_ref().ok_or("the plan has no [spot] section")?;
        let today = match params.date.as_deref() {
            Some(text) => chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
                .map_err(|_| format!("'{text}' is not a date like 2027-01-04"))?,
            None => chrono::Local::now().date_naive(),
        };
        let pair = spot_plan.symbol.clone();
        let candles = &self.candles;
        let (hours, days, weeks) = iced::futures::join!(
            candles.spot_candles(pair.clone(), Timeframe::H1, 2),
            candles.spot_candles(pair.clone(), Timeframe::D1, DAILY_BARS),
            candles.spot_candles(pair.clone(), Timeframe::W1, WEEKLY_BARS),
        );
        let hours = hours?;
        let close = hours
            .klines
            .last()
            .map(|kline| kline.close.to_f64())
            .ok_or("no candles")?;
        let day_closes = days.as_ref().map(closes).unwrap_or_default();
        let week_closes = weeks.as_ref().map(closes).unwrap_or_default();
        let mut notes = Vec::new();
        if let Err(error) = &days {
            notes.push(format!("rsi_1d: {error}"));
        }
        match &weeks {
            Err(error) => notes.push(format!("rsi_1w, sma200_1w: {error}")),
            Ok(_) if week_closes.len() < SMA_LENGTH => {
                notes.push(format!("sma200_1w: only {} weekly bars", week_closes.len()));
            }
            Ok(_) => {}
        }
        let status = spot::status(
            spot_plan,
            spot::Market {
                close,
                rsi_1d: math::rsi(&day_closes, RSI_LENGTH),
                rsi_1w: math::rsi(&week_closes, RSI_LENGTH),
                sma200_1w: math::sma(&week_closes, SMA_LENGTH),
            },
            today,
        );
        let mut out = serde_json::to_value(&status).map_err(|error| error.to_string())?;
        spot_rounded(&mut out, Precision::of(&hours.ticker_info));
        out["date"] = json!(today.to_string());
        out["as_of_bar_utc"] = json!(hours.klines.last().map(|k| utc(k.time.as_u64() as i64)));
        out["notes"] = json!(notes);
        to_text(&out)
    }

    #[tool(
        description = "Market snapshot of the plan's universe (or the coins given), USDT perpetuals, from closed candles: close (last closed hour), day % and week % (vs 24 h and 7 days ago), 24h high and low (at the report time just after 00:00 UTC this is the day that closed), RSI(14) on 1D, 4H and 1W, daily ATR(14), 1M high and low and 3M low, SMA200 on 1D and 1W with distance %, and the Roman Setup 12H bias with the plan's inputs. RSI and ATR use Wilder's smoothing like TradingView. Values without enough history are null and named in notes. A coin that fails gets an error row; the rest still return."
    )]
    pub async fn market_snapshot(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> Result<String, String> {
        use iced::futures::StreamExt;
        let plan = self.plan();
        let plan = match (&plan, &params.coins) {
            (Ok(plan), _) => Some(plan),
            (Err(error), None) => return Err(error.clone()),
            (Err(_), Some(_)) => None,
        };
        let (exchange, coins) = self.universe(params.coins, params.exchange.as_deref(), plan)?;
        let (_, roman) = self.indicator(Some(ROMAN))?;
        let rows: Vec<Json> = iced::futures::stream::iter(coins)
            .map(|coin| {
                let roman = &roman;
                async move {
                    self.coin_snapshot(exchange, coin.clone(), roman, plan)
                        .await
                        .unwrap_or_else(|error| json!({ "base": coin, "error": error }))
                }
            })
            .buffered(COINS_PARALLEL)
            .collect()
            .await;
        let failed = rows.iter().filter(|row| row.get("error").is_some()).count();
        to_text(&json!({
            "exchange": exchange_id(exchange),
            "plan_inputs": plan.is_some(),
            "coins": rows,
            "failed": failed,
        }))
    }
}

#[cfg(test)]
mod tests;
