//! Alert conditions raised by indicator plots declared with `alert = true`
//! on the open charts. Price alert lines are saved alerts
//! (`config::alert_store`), run by the alert engine.
//!
//! A chart pane owns one [`AlertWatch`]. It remembers, per indicator
//! instance, the newest closed bar it has already looked at. The first poll
//! after a chart loads only records that bar, so history never fires. Later
//! polls report alert plots that are `true` on bars that closed since then.
//!
//! A chart restored at launch can ask for a catch-up instead
//! ([`AlertWatch::catch_up_on_first_look`]): its first look also reports the
//! bars that closed at most one bar ago, so a signal on a bar that closed
//! while the app was off is not lost. The alert center labels those and
//! drops the ones it delivered before the restart.

use super::{IndicatorId, IndicatorInstanceId, IndicatorInstanceStore, KlineData, OutputValue};
use crate::chart::Basis;
use exchange::unit::price::Price;
use exchange::{TickerInfo, UnixMs};
use rustc_hash::FxHashMap;

/// Newly closed bars looked at in one poll. A longer gap (sleep, reconnect)
/// only reports its newest bars.
const MAX_CATCHUP_BARS: usize = 3;

/// What raised an alert.
#[derive(Debug, Clone, PartialEq)]
pub enum AlertSource {
    /// An indicator plot declared with `alert = true`.
    Indicator {
        indicator: IndicatorId,
        plot_id: String,
    },
}

/// One alert condition that turned true.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertHit {
    pub ticker_info: TickerInfo,
    pub basis: Basis,
    pub source: AlertSource,
    /// Indicator name.
    pub source_name: String,
    pub title: String,
    /// Open time of the closed bar.
    pub bar_time: UnixMs,
    /// Close of the bar.
    pub close: Price,
}

impl AlertHit {
    /// Stable key of the condition, used for muting in settings.
    pub fn condition_key(&self) -> String {
        match &self.source {
            AlertSource::Indicator { indicator, plot_id } => condition_key(indicator, plot_id),
        }
    }
}

pub fn condition_key(indicator: &IndicatorId, plot_id: &str) -> String {
    format!("{}/{plot_id}", indicator.id())
}

#[derive(Debug, Default)]
pub struct AlertWatch {
    scope: Option<(TickerInfo, Basis)>,
    checked: FxHashMap<IndicatorInstanceId, UnixMs>,
    /// The first look of each indicator reports recent bars too. Cleared
    /// once every indicator has looked, or when the chart's ticker or
    /// timeframe changes.
    catch_up: bool,
}

impl AlertWatch {
    /// Forgets every baseline, so the next poll starts fresh.
    pub fn reset(&mut self) {
        self.scope = None;
        self.checked.clear();
        self.catch_up = false;
    }

    /// For a chart restored at launch: its first look reports bars that
    /// closed at most one bar ago instead of only setting the baseline.
    pub fn catch_up_on_first_look(&mut self) {
        self.catch_up = true;
    }

    /// Reports alert plots of `active` indicators that turned true on bars
    /// closed since the last poll. Indicators for which `ready` is false (for
    /// example while they are being recalculated) keep their baseline and are
    /// looked at again on a later poll.
    pub fn poll(
        &mut self,
        data: &KlineData,
        instances: &IndicatorInstanceStore,
        active: &[IndicatorInstanceId],
        ready: impl Fn(IndicatorInstanceId) -> bool,
        now: UnixMs,
    ) -> Vec<AlertHit> {
        let Some(series) = data.base_series() else {
            self.reset();
            return Vec::new();
        };
        let scope = (data.base_ticker(), series.basis());
        if self.scope != Some(scope) {
            // The first scope keeps a catch-up; a ticker or timeframe change
            // does not.
            let catch_up = self.catch_up && self.scope.is_none();
            self.reset();
            self.scope = Some(scope);
            self.catch_up = catch_up;
        }
        self.checked.retain(|id, _| active.contains(id));

        let mut closed = Vec::with_capacity(MAX_CATCHUP_BARS);
        for point in series.points().rev().skip(1).take(MAX_CATCHUP_BARS) {
            closed.push(ClosedBar {
                index: point.index,
                time: point.kline.time,
                close: point.kline.close,
            });
        }
        closed.reverse();

        let bar_ms = match scope.1 {
            Basis::Time(timeframe) => Some(timeframe.to_milliseconds()),
            Basis::Tick(_) => None,
        };
        let host = data.indicator_host();
        let mut hits = Vec::new();
        let mut waiting = false;
        for id in active {
            if !ready(*id) {
                waiting = true;
                continue;
            }
            let Some(instance) = instances.get(*id) else {
                continue;
            };
            let definition = instance.kind.definition();
            let plots = definition
                .outputs
                .iter()
                .filter(|output| output.alert)
                .collect::<Vec<_>>();
            if plots.is_empty() {
                continue;
            }
            let found = scan(
                self.checked.get(id).copied(),
                &closed,
                plots.len(),
                |plot, index| host.output_value_for(*id, &plots[plot].id, index),
                bar_ms.map(|bar_ms| (now, bar_ms)),
                self.catch_up,
            );
            match found.checked {
                Some(time) => {
                    self.checked.insert(*id, time);
                }
                None => waiting = true,
            }
            for (plot, bar) in found.hits {
                hits.push(AlertHit {
                    ticker_info: scope.0,
                    basis: scope.1,
                    source: AlertSource::Indicator {
                        indicator: instance.kind.clone(),
                        plot_id: plots[plot].id.clone(),
                    },
                    source_name: definition.display_name.clone(),
                    title: plots[plot].label.clone(),
                    bar_time: bar.time,
                    close: bar.close,
                });
            }
        }
        if !waiting {
            self.catch_up = false;
        }
        hits
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ClosedBar {
    index: usize,
    time: UnixMs,
    close: Price,
}

#[derive(Debug, PartialEq)]
struct ScanResult {
    checked: Option<UnixMs>,
    hits: Vec<(usize, ClosedBar)>,
}

/// Looks at the newest closed bars (`closed`, oldest first) for one indicator
/// instance. `value(plot, index)` reads an alert plot; `None` means the
/// indicator has not produced it yet. `recency` is `(now, bar length)` for
/// time-based charts: bars that closed more than one bar length ago are
/// recorded but not reported. With `catch_up`, a first look reports those
/// recent bars instead of only setting the baseline (time-based charts only).
fn scan(
    checked: Option<UnixMs>,
    closed: &[ClosedBar],
    plot_count: usize,
    value: impl Fn(usize, usize) -> Option<OutputValue>,
    recency: Option<(UnixMs, u64)>,
    catch_up: bool,
) -> ScanResult {
    let Some(newest) = closed.last().copied() else {
        return ScanResult {
            checked,
            hits: Vec::new(),
        };
    };
    let ready = (0..plot_count).any(|plot| value(plot, newest.index).is_some());
    if !ready {
        return ScanResult {
            checked,
            hits: Vec::new(),
        };
    }
    let checked = match checked.filter(|checked| *checked <= newest.time) {
        Some(checked) => Some(checked),
        // First look with a catch-up: every recent bar counts.
        None if catch_up && checked.is_none() && recency.is_some() => None,
        // First look, or the history was replaced: set the baseline.
        None => {
            return ScanResult {
                checked: Some(newest.time),
                hits: Vec::new(),
            };
        }
    };
    let mut hits = Vec::new();
    for bar in closed
        .iter()
        .filter(|bar| checked.is_none_or(|checked| bar.time > checked))
    {
        if let Some((now, bar_ms)) = recency {
            let closed_at = bar.time.as_u64().saturating_add(bar_ms);
            if now.as_u64().saturating_sub(closed_at) > bar_ms {
                continue;
            }
        }
        for plot in 0..plot_count {
            if value(plot, bar.index) == Some(OutputValue::Boolean(true)) {
                hits.push((plot, *bar));
            }
        }
    }
    ScanResult {
        checked: Some(newest.time),
        hits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;

    fn bars(times: &[u64]) -> Vec<ClosedBar> {
        times
            .iter()
            .enumerate()
            .map(|(index, time)| ClosedBar {
                index,
                time: UnixMs(*time),
                close: Price::from_f64(100.0 + index as f64),
            })
            .collect()
    }

    /// Plot 0 is true on the given bar indices, plot 1 never.
    fn values(true_on: &'static [usize]) -> impl Fn(usize, usize) -> Option<OutputValue> {
        move |plot, index| Some(OutputValue::Boolean(plot == 0 && true_on.contains(&index)))
    }

    #[test]
    fn first_poll_only_sets_baseline() {
        let closed = bars(&[HOUR, 2 * HOUR, 3 * HOUR]);
        let result = scan(None, &closed, 2, values(&[0, 1, 2]), None, false);
        assert_eq!(result.checked, Some(UnixMs(3 * HOUR)));
        assert!(result.hits.is_empty());
    }

    #[test]
    fn new_closed_bar_fires_once() {
        let closed = bars(&[HOUR, 2 * HOUR, 3 * HOUR]);
        let result = scan(
            Some(UnixMs(2 * HOUR)),
            &closed,
            2,
            values(&[2]),
            None,
            false,
        );
        assert_eq!(result.checked, Some(UnixMs(3 * HOUR)));
        assert_eq!(result.hits, vec![(0, closed[2])]);

        let again = scan(result.checked, &closed, 2, values(&[2]), None, false);
        assert!(again.hits.is_empty());
    }

    #[test]
    fn false_and_old_bars_do_not_fire() {
        let closed = bars(&[HOUR, 2 * HOUR, 3 * HOUR]);
        // Bar 1 is true but was already checked; bar 2 is false.
        let result = scan(
            Some(UnixMs(2 * HOUR)),
            &closed,
            2,
            values(&[1]),
            None,
            false,
        );
        assert!(result.hits.is_empty());
        assert_eq!(result.checked, Some(UnixMs(3 * HOUR)));
    }

    #[test]
    fn waits_until_indicator_has_values() {
        let closed = bars(&[HOUR, 2 * HOUR]);
        let result = scan(Some(UnixMs(HOUR)), &closed, 1, |_, _| None, None, false);
        assert_eq!(result.checked, Some(UnixMs(HOUR)));
        assert!(result.hits.is_empty());

        let later = scan(result.checked, &closed, 1, values(&[1]), None, false);
        assert_eq!(later.hits, vec![(0, closed[1])]);
    }

    #[test]
    fn replaced_history_resets_baseline() {
        let closed = bars(&[HOUR, 2 * HOUR]);
        let result = scan(
            Some(UnixMs(9 * HOUR)),
            &closed,
            1,
            values(&[1]),
            None,
            false,
        );
        assert_eq!(result.checked, Some(UnixMs(2 * HOUR)));
        assert!(result.hits.is_empty());
    }

    #[test]
    fn stale_bars_are_skipped() {
        let closed = bars(&[HOUR, 2 * HOUR, 3 * HOUR]);
        // Bar 2 closed at 4h. At 5h it is one bar old and still reported, at
        // 5h + 1ms it is stale; bar 1 (closed at 3h) is stale in both cases.
        let fresh = scan(
            Some(UnixMs(HOUR)),
            &closed,
            1,
            values(&[1, 2]),
            Some((UnixMs(5 * HOUR), HOUR)),
            false,
        );
        assert_eq!(fresh.hits, vec![(0, closed[2])]);
        let stale = scan(
            Some(UnixMs(HOUR)),
            &closed,
            1,
            values(&[1, 2]),
            Some((UnixMs(5 * HOUR + 1), HOUR)),
            false,
        );
        assert!(stale.hits.is_empty());
        assert_eq!(stale.checked, Some(UnixMs(3 * HOUR)));
    }

    #[test]
    fn catch_up_reports_recent_bars_on_the_first_look() {
        let closed = bars(&[HOUR, 2 * HOUR, 3 * HOUR]);
        // At 4h 30m bar 2 (closed at 4h) is recent, bar 1 (closed at 3h) is
        // more than one bar old.
        let recency = Some((UnixMs(4 * HOUR + HOUR / 2), HOUR));
        let result = scan(None, &closed, 1, values(&[1, 2]), recency, true);
        assert_eq!(result.hits, vec![(0, closed[2])]);
        assert_eq!(result.checked, Some(UnixMs(3 * HOUR)));

        // Without a catch-up, or on a tick chart, the first look stays quiet.
        assert!(
            scan(None, &closed, 1, values(&[2]), recency, false)
                .hits
                .is_empty()
        );
        assert!(
            scan(None, &closed, 1, values(&[2]), None, true)
                .hits
                .is_empty()
        );
        // Replaced history is not a first look.
        let replaced = scan(
            Some(UnixMs(9 * HOUR)),
            &closed,
            1,
            values(&[2]),
            recency,
            true,
        );
        assert!(replaced.hits.is_empty());
    }
}
