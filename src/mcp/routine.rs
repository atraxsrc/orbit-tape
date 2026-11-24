//! MCP tools for the daily routine: the user's plan, a market snapshot of
//! the plan's universe, the Roman setups and the spot status built on it.

mod math;

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
