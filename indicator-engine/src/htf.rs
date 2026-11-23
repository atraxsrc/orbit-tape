//! Higher-timeframe candles aggregated from the chart's own bars.
//!
//! `htf.bars`, `htf.new_bar` and `htf.forming` bucket chart bars into candles of
//! a larger timeframe by UTC time. A higher-timeframe candle counts as closed
//! once the first chart bar of the next bucket arrives, so closed values appear
//! on that bar and never change afterwards (Pine's `expr[1]` with
//! `lookahead_on`). Only `htf.forming` exposes the unfinished candle.

use crate::hir::CallSiteId;
use crate::intrinsic::{IntrinsicError, IntrinsicEvalContext, IntrinsicState};
use crate::types::{ArrayValue, Value};
use chrono::{Datelike, TimeZone, Utc};
use std::collections::{BTreeMap, VecDeque};
use std::mem::size_of;
use std::sync::Arc;

const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;
const WEEK: i64 = 7 * DAY;
/// 1970-01-05, the first Monday after the Unix epoch.
const FIRST_MONDAY: i64 = 4 * DAY;

/// A higher timeframe, parsed from a Pine-style string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Period {
    /// Intraday candles of `ms`, restarting at every UTC midnight like
    /// TradingView's crypto sessions.
    Intraday { ms: i64 },
    /// Whole days, counted from the Unix epoch.
    Days { ms: i64 },
    /// Weeks starting Monday 00:00 UTC.
    Week,
    /// Calendar months.
    Month,
}

impl Period {
    /// Parses `"<minutes>"`, `"<n>H"`, `"D"`/`"<n>D"`, `"W"`/`"1W"` and
    /// `"M"`/`"1M"`, case-insensitively.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_uppercase();
        let (count, unit) = match text.find(|c: char| !c.is_ascii_digit()) {
            Some(0) => (1, text.as_str()),
            Some(split) => (text[..split].parse::<i64>().ok()?, &text[split..]),
            None => (text.parse::<i64>().ok()?, ""),
        };
        if count <= 0 {
            return None;
        }
        match unit {
            "" => Self::from_minutes(count),
            "H" => Self::from_minutes(count.checked_mul(60)?),
            "D" => Some(Self::Days {
                ms: count.checked_mul(DAY)?,
            }),
            "W" if count == 1 => Some(Self::Week),
            "M" if count == 1 => Some(Self::Month),
            _ => None,
        }
    }

    fn from_minutes(minutes: i64) -> Option<Self> {
        let ms = minutes.checked_mul(MINUTE)?;
        if ms < DAY {
            Some(Self::Intraday { ms })
        } else if ms % DAY == 0 {
            Some(Self::Days { ms })
        } else {
            None
        }
    }

    /// Start of the bucket holding `time` (Unix milliseconds).
    pub(crate) fn bucket_start(self, time: i64) -> Option<i64> {
        match self {
            Self::Intraday { ms } => {
                let day = time.div_euclid(DAY) * DAY;
                Some(day + (time - day).div_euclid(ms) * ms)
            }
            Self::Days { ms } => Some(time.div_euclid(ms) * ms),
            Self::Week => Some((time - FIRST_MONDAY).div_euclid(WEEK) * WEEK + FIRST_MONDAY),
            Self::Month => {
                let date = Utc.timestamp_millis_opt(time).single()?.date_naive();
                let first = date.with_day(1)?.and_hms_opt(0, 0, 0)?;
                Some(first.and_utc().timestamp_millis())
            }
        }
    }

    /// Start of the bucket `count` buckets before the one holding `time`.
    pub(crate) fn history_start(self, time: i64, count: usize) -> Option<i64> {
        let mut start = self.bucket_start(time)?;
        for _ in 0..count {
            start = self.bucket_start(start.checked_sub(1)?)?;
        }
        Some(start)
    }
}

/// One higher-timeframe candle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Candle {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

impl Candle {
    fn to_value(self) -> Value {
        let float = |value: f64| {
            if value.is_finite() {
                Value::Float(value)
            } else {
                Value::Na
            }
        };
        let mut fields = BTreeMap::new();
        fields.insert("time".to_owned(), Value::Int(self.time));
        fields.insert("open".to_owned(), float(self.open));
        fields.insert("high".to_owned(), float(self.high));
        fields.insert("low".to_owned(), float(self.low));
        fields.insert("close".to_owned(), float(self.close));
        fields.insert("volume".to_owned(), float(self.volume));
        Value::Record(fields)
    }
}

/// The chart bar being evaluated.
#[derive(Debug, Clone, Copy)]
struct Bar {
    index: usize,
    time: i64,
    ohlcv: [Option<f64>; 5],
}

/// Per-call-site aggregation state.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HtfState {
    period: Period,
    /// Start of the bucket the last chart bar fell into.
    bucket: Option<i64>,
    /// Whether the current bucket began before the first chart bar seen, so its
    /// candle is incomplete and must not be reported as closed.
    partial: bool,
    forming: Option<Candle>,
    /// Closed candles, newest first.
    closed: VecDeque<Candle>,
    /// How many closed candles to keep.
    capacity: usize,
    /// Whether the last chart bar opened a new bucket after a closed one.
    new_bar: bool,
    /// Chart bar index of the last update, so a site updates once per bar.
    last_index: Option<usize>,
}

impl HtfState {
    fn new(period: Period) -> Self {
        Self {
            period,
            bucket: None,
            partial: false,
            forming: None,
            closed: VecDeque::new(),
            capacity: 0,
            new_bar: false,
            last_index: None,
        }
    }

    fn update(&mut self, bar: Bar) {
        if self.last_index == Some(bar.index) {
            return;
        }
        self.last_index = Some(bar.index);

        let Some(start) = self.period.bucket_start(bar.time) else {
            return;
        };

        self.new_bar = false;
        if self.bucket != Some(start) {
            let first_bucket = self.bucket.is_none();
            let finished = self.forming.take();
            if let Some(candle) = finished.filter(|_| !self.partial) {
                self.closed.push_front(candle);
                self.closed.truncate(self.capacity);
                self.new_bar = true;
            }
            self.bucket = Some(start);
            self.partial = first_bucket && bar.time > start;
        }

        let [open, high, low, close, volume] = bar.ohlcv;
        let (Some(open), Some(high), Some(low), Some(close)) = (open, high, low, close) else {
            return;
        };
        let volume = volume.unwrap_or(0.0);
        match &mut self.forming {
            Some(candle) => {
                candle.high = candle.high.max(high);
                candle.low = candle.low.min(low);
                candle.close = close;
                candle.volume += volume;
            }
            None => {
                self.forming = Some(Candle {
                    time: start,
                    open,
                    high,
                    low,
                    close,
                    volume,
                });
            }
        }
    }

    pub(crate) fn copy_work(&self) -> usize {
        self.closed.len().saturating_add(1)
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.closed.capacity().saturating_mul(size_of::<Candle>()))
    }
}

fn period_argument(args: &[Value]) -> Option<Period> {
    match args.first() {
        Some(Value::String(text)) => Period::parse(text),
        _ => None,
    }
}

/// The site's state, updated with the current chart bar. `None` when the
/// timeframe can't be parsed.
fn updated_state<'s>(
    context: &'s mut IntrinsicEvalContext<'_>,
    site: CallSiteId,
    args: &[Value],
    capacity: usize,
) -> Result<Option<&'s HtfState>, IntrinsicError> {
    let Some(period) = period_argument(args) else {
        return Ok(None);
    };
    let bar = Bar {
        index: context.bar_index,
        time: context.bar_time,
        ohlcv: context.bar_ohlcv,
    };
    let state = context
        .states
        .entry(site)
        .or_insert_with(|| IntrinsicState::Htf(Box::new(HtfState::new(period))));
    let IntrinsicState::Htf(state) = state else {
        return Err(IntrinsicError::InvalidState(
            "htf call site has incompatible state",
        ));
    };
    if state.period != period {
        **state = HtfState::new(period);
    }
    state.capacity = state.capacity.max(capacity);
    state.update(bar);
    Ok(Some(state))
}

/// `htf.bars(timeframe, count)`: up to `count` closed candles, newest first.
pub(crate) fn eval_bars(
    context: &mut IntrinsicEvalContext<'_>,
    site: CallSiteId,
    args: &[Value],
) -> Result<Value, IntrinsicError> {
    let count = crate::intrinsic::window_length(context, args, 1)?;
    let max_elements = context.max_collection_elements;
    let max_heap = context.max_heap_bytes;
    let Some(state) = updated_state(context, site, args, count)? else {
        return Ok(Value::Na);
    };
    let values: Vec<Value> = state
        .closed
        .iter()
        .take(count)
        .map(|candle| candle.to_value())
        .collect();

    context.charge(values.len().saturating_mul(8).saturating_add(1))?;
    let array = ArrayValue::new(values);
    let (elements, heap_bytes) = array.resource_usage();
    if elements > max_elements {
        return Err(IntrinsicError::LimitExceeded("collection elements"));
    }
    if heap_bytes > max_heap {
        return Err(IntrinsicError::LimitExceeded("heap bytes"));
    }
    Ok(Value::Array(Arc::new(array)))
}

/// `htf.new_bar(timeframe)`: true on the first chart bar after a candle closed.
pub(crate) fn eval_new_bar(
    context: &mut IntrinsicEvalContext<'_>,
    site: CallSiteId,
    args: &[Value],
) -> Result<Value, IntrinsicError> {
    context.charge(1)?;
    Ok(match updated_state(context, site, args, 0)? {
        Some(state) => Value::Bool(state.new_bar),
        None => Value::Na,
    })
}

/// `htf.forming(timeframe)`: the unfinished candle. Repaints by design.
pub(crate) fn eval_forming(
    context: &mut IntrinsicEvalContext<'_>,
    site: CallSiteId,
    args: &[Value],
) -> Result<Value, IntrinsicError> {
    context.charge(8)?;
    Ok(updated_state(context, site, args, 0)?
        .and_then(|state| state.forming)
        .map_or(Value::Na, Candle::to_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 60 * MINUTE;
    /// 2024-01-01 00:00 UTC, a Monday.
    const JAN_1_2024: i64 = 1_704_067_200_000;

    #[test]
    fn parses_timeframes() {
        assert_eq!(
            Period::parse("720"),
            Some(Period::Intraday { ms: 12 * HOUR })
        );
        assert_eq!(
            Period::parse("12h"),
            Some(Period::Intraday { ms: 12 * HOUR })
        );
        assert_eq!(Period::parse("60"), Some(Period::Intraday { ms: HOUR }));
        assert_eq!(Period::parse("1440"), Some(Period::Days { ms: DAY }));
        assert_eq!(Period::parse("D"), Some(Period::Days { ms: DAY }));
        assert_eq!(Period::parse("3D"), Some(Period::Days { ms: 3 * DAY }));
        assert_eq!(Period::parse("W"), Some(Period::Week));
        assert_eq!(Period::parse("1w"), Some(Period::Week));
        assert_eq!(Period::parse("M"), Some(Period::Month));
        for bad in ["", "0", "-5", "2W", "2M", "1500", "abc", "12X"] {
            assert_eq!(Period::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn buckets_align_to_utc() {
        let h12 = Period::parse("720").unwrap();
        assert_eq!(h12.bucket_start(JAN_1_2024 + 11 * HOUR), Some(JAN_1_2024));
        assert_eq!(
            h12.bucket_start(JAN_1_2024 + 12 * HOUR),
            Some(JAN_1_2024 + 12 * HOUR)
        );

        // 5H does not divide a day: candles restart at midnight (20:00-24:00 is
        // a short last candle), as on TradingView crypto charts.
        let h5 = Period::parse("300").unwrap();
        assert_eq!(
            h5.bucket_start(JAN_1_2024 + 23 * HOUR),
            Some(JAN_1_2024 + 20 * HOUR)
        );
        assert_eq!(
            h5.bucket_start(JAN_1_2024 + 24 * HOUR + HOUR),
            Some(JAN_1_2024 + 24 * HOUR)
        );

        let week = Period::Week;
        // Sunday 2024-01-07 23:00 belongs to the week of Monday 2024-01-01.
        assert_eq!(
            week.bucket_start(JAN_1_2024 + 6 * DAY + 23 * HOUR),
            Some(JAN_1_2024)
        );
        assert_eq!(
            week.bucket_start(JAN_1_2024 + 7 * DAY),
            Some(JAN_1_2024 + 7 * DAY)
        );

        // 2024-02-15 -> 2024-02-01.
        let feb_1 = JAN_1_2024 + 31 * DAY;
        assert_eq!(Period::Month.bucket_start(feb_1 + 14 * DAY), Some(feb_1));

        // Before the epoch still floors downwards.
        assert_eq!(Period::Days { ms: DAY }.bucket_start(-1), Some(-DAY));
    }

    #[test]
    fn history_start_steps_back_whole_buckets() {
        let h12 = Period::parse("720").unwrap();
        let now = JAN_1_2024 + 10 * DAY + 5 * HOUR;
        assert_eq!(h12.history_start(now, 0), Some(JAN_1_2024 + 10 * DAY));
        assert_eq!(
            h12.history_start(now, 3),
            Some(JAN_1_2024 + 10 * DAY - 36 * HOUR)
        );
        let mar_1 = JAN_1_2024 + 60 * DAY; // 2024 is a leap year
        assert_eq!(
            Period::Month.history_start(mar_1 + DAY, 2),
            Some(JAN_1_2024)
        );
    }
}
