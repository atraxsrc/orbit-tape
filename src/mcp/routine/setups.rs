//! The daily routine's Roman Setup rules: from one coin's x_* setup values
//! and a few market numbers to the bias-aligned trade levels and the coin's
//! status (LIVE, WATCH, FLIP-WATCH, COUNTER-TREND).
//!
//! Prices only; no orders. The thresholds are the routine's own:
//! a tap within the last [`LIVE_BARS`] 1h bars, WATCH within [`WATCH_PCT`],
//! FLIP-WATCH within [`FLIP_PCT`], stops [`STOP_ATR`] (counter-trend
//! [`CT_STOP_ATR`]) daily ATR beyond the level.

use serde::Serialize;
use std::collections::BTreeMap;

pub const LIVE_BARS: f64 = 24.0;
pub const WATCH_PCT: f64 = 5.0;
pub const FLIP_PCT: f64 = 3.0;
pub const STOP_ATR: f64 = 0.3;
pub const CT_STOP_ATR: f64 = 0.2;
/// Size is given per this much risked.
pub const RISK_UNIT: f64 = 10.0;

/// What the rules need about one coin.
#[derive(Debug, Clone, Default)]
pub struct CoinInput {
    pub close: f64,
    /// Daily ATR(14).
    pub atr: Option<f64>,
    pub high_24h: Option<f64>,
    pub low_24h: Option<f64>,
    pub high_1m: Option<f64>,
    pub low_1m: Option<f64>,
    /// Roman Setup's x_* values of the newest closed 1h bar.
    pub values: BTreeMap<String, Option<f64>>,
}

/// The plan's prop rules for one coin.
#[derive(Debug, Clone, Copy)]
pub struct Rules {
    pub max_stop_pct: f64,
    pub leverage: f64,
    pub risk_pct: f64,
    pub counter_trend_risk_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    fn role(self) -> &'static str {
        match self {
            Self::Long => "support",
            Self::Short => "resistance",
        }
    }

    fn sign(self) -> f64 {
        match self {
            Self::Long => 1.0,
            Self::Short => -1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelKind {
    OrderBlock,
    Breaker,
    BrokenLevel,
}

/// A support or resistance on the trade's side of price.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Level {
    pub kind: LevelKind,
    /// What it is in words, e.g. "bear-slot breaker (broken bear OB)": a
    /// long can come from a bear slot when its order block was broken.
    pub name: &'static str,
    /// support (backs a long) or resistance (backs a short).
    pub role: &'static str,
    /// The slot it came from (b1, b2, s1, s2), for zones.
    pub slot: Option<&'static str>,
    /// The edge price reaches first.
    pub near: f64,
    /// The other edge (the same price for a broken level).
    pub far: f64,
    /// Out of the indicator's range (shown hidden on the chart).
    pub hidden: bool,
    /// From the close to the near edge, %; 0 when price is inside.
    pub distance_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trade {
    pub direction: Direction,
    pub entry: f64,
    /// From the close to the entry, %.
    pub entry_distance_pct: f64,
    pub stop: f64,
    pub stop_pct: f64,
    pub t1: Option<f64>,
    pub t1_r: Option<f64>,
    pub t2: Option<f64>,
    pub t2_r: Option<f64>,
    /// Position size (notional) when [`RISK_UNIT`] is risked.
    pub notional_per_10: f64,
    /// Margin for that notional at the prop's max leverage.
    pub margin_per_10: f64,
    pub max_leverage: f64,
    pub risk_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Flip {
    /// The bias a 12H close beyond the level flips to.
    pub to: &'static str,
    pub level: f64,
    pub distance_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CounterTrend {
    pub direction: Direction,
    /// Entry comes after the next 1h MSS this way; the numbers below use
    /// the close.
    pub entry_rule: &'static str,
    pub stop: f64,
    pub stop_pct: Option<f64>,
    pub t1: Option<f64>,
    pub t1_r: Option<f64>,
    pub notional_per_10: Option<f64>,
    pub risk_pct: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING-KEBAB-CASE")]
pub enum Status {
    Live,
    /// Triggered like LIVE, but price already ran: past T1, or further past
    /// the entry than the stop distance. Not a trade to take now.
    Ran,
    Watch,
    FlipWatch,
    CounterTrend,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Setup {
    pub bias: &'static str,
    pub statuses: Vec<Status>,
    pub primary: Option<Level>,
    pub trade: Option<Trade>,
    /// Why there is no trade (no bias, no level, stop too wide, ...).
    pub skipped: Option<String>,
    pub flip: Option<Flip>,
    pub counter_trend: Option<CounterTrend>,
    pub tap_ago: Option<f64>,
    pub mss_ago: Option<f64>,
    /// Why a triggered setup is RAN, not LIVE.
    pub ran: Option<String>,
}

fn pct(from: f64, to: f64) -> f64 {
    (to - from).abs() / from * 100.0
}

impl Trade {
    /// Why price has already run from this trade at `close`: at or past T1,
    /// or further past the entry (the profit way) than the stop distance.
    pub fn ran(&self, close: f64) -> Option<String> {
        let sign = self.direction.sign();
        if let Some(t1) = self.t1
            && (close - t1) * sign >= 0.0
        {
            return Some(format!("price is at or past T1 ({t1})"));
        }
        let risk = (self.entry - self.stop).abs();
        let run = (close - self.entry) * sign;
        (run > risk).then(|| {
            format!(
                "price is {:.2}% past the entry, more than the {:.2}% stop distance",
                run / self.entry * 100.0,
                self.stop_pct
            )
        })
    }
}

impl CoinInput {
    fn value(&self, name: &str) -> Option<f64> {
        self.values.get(name).copied().flatten()
    }

    /// Zones and the broken level that back a trade `direction`, on its side
    /// of price, nearest first.
    pub fn levels(&self, direction: Direction) -> Vec<Level> {
        let close = self.close;
        let mut levels = Vec::new();
        for (slot, bull) in [("b1", true), ("b2", true), ("s1", false), ("s2", false)] {
            let (Some(top), Some(bottom), Some(state)) = (
                self.value(&format!("x_{slot}Top")),
                self.value(&format!("x_{slot}Bot")),
                self.value(&format!("x_{slot}St")),
            ) else {
                continue;
            };
            let state = state as i64;
            let breaker = state % 2 == 1;
            // A bull order block or a bear breaker supports; the others resist.
            let support = bull != breaker;
            let wanted = match direction {
                Direction::Long => support && bottom <= close,
                Direction::Short => !support && top >= close,
            };
            if !wanted {
                continue;
            }
            let (near, far) = match direction {
                Direction::Long => (top, bottom),
                Direction::Short => (bottom, top),
            };
            let inside = bottom <= close && close <= top;
            levels.push(Level {
                kind: if breaker {
                    LevelKind::Breaker
                } else {
                    LevelKind::OrderBlock
                },
                name: match (bull, breaker) {
                    (true, false) => "bull order block",
                    (false, false) => "bear order block",
                    (true, true) => "bull-slot breaker (broken bull OB)",
                    (false, true) => "bear-slot breaker (broken bear OB)",
                },
                role: direction.role(),
                slot: Some(slot),
                near,
                far,
                hidden: state >= 2,
                distance_pct: if inside { 0.0 } else { pct(close, near) },
            });
        }
        if let Some(broken) = self.value("x_brkLvl") {
            let on_side = match direction {
                Direction::Long => broken <= close,
                Direction::Short => broken >= close,
            };
            if on_side {
                levels.push(Level {
                    kind: LevelKind::BrokenLevel,
                    name: "broken HTF level",
                    role: direction.role(),
                    slot: None,
                    near: broken,
                    far: broken,
                    hidden: false,
                    distance_pct: pct(close, broken),
                });
            }
        }
        levels.sort_by(|a, b| a.distance_pct.total_cmp(&b.distance_pct));
        levels
    }

    /// Entry, stop and targets for a trade at `level`.
    pub fn trade(
        &self,
        direction: Direction,
        level: &Level,
        rules: &Rules,
    ) -> Result<Trade, String> {
        let close = self.close;
        let sign = direction.sign();
        let atr = self.atr.ok_or("no daily ATR (not enough daily candles)")?;
        let fib = self.value("x_fib50");
        // Candidates price reaches first: the near edge, the broken level and
        // the 50% fib, on the trade's side of price.
        let on_side = |price: f64| (close - price) * sign >= 0.0;
        let inside = level.distance_pct == 0.0 && level.kind != LevelKind::BrokenLevel;
        let mut candidates = vec![level.near];
        candidates.extend(self.value("x_brkLvl"));
        candidates.extend(fib);
        let entry = if inside {
            close
        } else {
            candidates
                .into_iter()
                .filter(|price| on_side(*price))
                .min_by(|a, b| (close - a).abs().total_cmp(&(close - b).abs()))
                .unwrap_or(level.near)
        };
        // The area: the level, the entry, and the fib when it is the entry or
        // lies inside the zone.
        let (low, high) = (level.near.min(level.far), level.near.max(level.far));
        let mut area = vec![level.near, level.far, entry];
        if let Some(fib) = fib
            && (fib == entry || (low..=high).contains(&fib))
        {
            area.push(fib);
        }
        let beyond = match direction {
            Direction::Long => area.into_iter().fold(f64::MAX, f64::min),
            Direction::Short => area.into_iter().fold(f64::MIN, f64::max),
        };
        let stop = beyond - sign * STOP_ATR * atr;
        let risk = (entry - stop).abs();
        if risk <= 0.0 {
            return Err("entry and stop are the same price".to_owned());
        }
        let stop_pct = risk / entry * 100.0;
        if stop_pct > rules.max_stop_pct {
            return Err(format!(
                "stop {stop_pct:.2}% from entry is over the plan's max {}%",
                rules.max_stop_pct
            ));
        }
        let reward = |target: Option<f64>| {
            target
                .map(|target| (target - entry) * sign / risk)
                .filter(|r| *r > 0.0)
        };
        let (t1, t2) = match direction {
            Direction::Long => (self.high_24h, self.high_1m),
            Direction::Short => (self.low_24h, self.low_1m),
        };
        let notional = RISK_UNIT / (stop_pct / 100.0);
        Ok(Trade {
            direction,
            entry,
            entry_distance_pct: pct(close, entry),
            stop,
            stop_pct,
            t1,
            t1_r: reward(t1),
            t2,
            t2_r: reward(t2),
            notional_per_10: notional,
            margin_per_10: notional / rules.leverage,
            max_leverage: rules.leverage,
            risk_pct: rules.risk_pct,
        })
    }

    /// The level a 12H close must cross to flip the bias.
    pub fn flip(&self, bias: i64) -> Option<Flip> {
        let (to, level) = match bias {
            -1 => ("bullish", self.value("x_bullFlip")?),
            1 => ("bearish", self.value("x_bearFlip")?),
            _ => return None,
        };
        Some(Flip {
            to,
            level,
            distance_pct: pct(self.close, level),
        })
    }

    /// An armed counter-trend trade (an HTF SFP against the bias).
    pub fn counter_trend(&self, rules: &Rules) -> Option<CounterTrend> {
        let direction = match self.value("x_ctArm")? {
            arm if arm > 0.0 => Direction::Long,
            arm if arm < 0.0 => Direction::Short,
            _ => return None,
        };
        let wick = self.value("x_ctWick")?;
        let sign = direction.sign();
        let stop = wick - sign * CT_STOP_ATR * self.atr.unwrap_or(0.0);
        let close = self.close;
        // Target: the broken level, else the nearest opposite zone.
        let target = self
            .value("x_brkLvl")
            .filter(|level| (level - close) * sign > 0.0)
            .or_else(|| {
                let opposite = match direction {
                    Direction::Long => Direction::Short,
                    Direction::Short => Direction::Long,
                };
                self.levels(opposite)
                    .into_iter()
                    .find(|level| level.distance_pct > 0.0)
                    .map(|level| level.near)
            });
        let risk = (close - stop) * sign;
        let valid = risk > 0.0;
        Some(CounterTrend {
            direction,
            entry_rule: match direction {
                Direction::Long => "after the next 1h MSS up",
                Direction::Short => "after the next 1h MSS down",
            },
            stop,
            stop_pct: valid.then(|| risk / close * 100.0),
            t1: target,
            t1_r: target
                .filter(|_| valid)
                .map(|target| (target - close) * sign / risk)
                .filter(|r| *r > 0.0),
            notional_per_10: valid.then(|| RISK_UNIT / (risk / close)),
            risk_pct: rules.counter_trend_risk_pct,
        })
    }

    /// The whole setup of one coin.
    pub fn setup(&self, rules: &Rules) -> Setup {
        let bias = self.value("x_bias").unwrap_or(0.0) as i64;
        let flip = self.flip(bias);
        let counter_trend = self.counter_trend(rules);
        let direction = match bias {
            1 => Some(Direction::Long),
            -1 => Some(Direction::Short),
            _ => None,
        };
        let (tap_name, mss_dir) = match direction {
            Some(Direction::Long) => ("x_tapUpAgo", 1.0),
            _ => ("x_tapDnAgo", -1.0),
        };
        let tap_ago = self.value(tap_name);
        let mss_ago = self.value("x_mssAgo");
        let mut statuses = Vec::new();
        let mut primary = None;
        let mut trade = None;
        let mut skipped = None;
        let mut ran = None;
        match direction {
            None => skipped = Some("no HTF bias yet".to_owned()),
            Some(direction) => match self.levels(direction).into_iter().next() {
                None => skipped = Some("no zone or broken level on the bias side".to_owned()),
                Some(level) => {
                    primary = Some(level);
                    match self.trade(direction, &level, rules) {
                        Ok(found) => {
                            let live = matches!(
                                (tap_ago, self.value("x_mssDir"), mss_ago),
                                (Some(tap), Some(dir), Some(mss))
                                    if tap <= LIVE_BARS && dir == mss_dir && mss <= tap
                            );
                            ran = live.then(|| found.ran(self.close)).flatten();
                            if ran.is_some() {
                                statuses.push(Status::Ran);
                            } else if live {
                                statuses.push(Status::Live);
                            } else if level.distance_pct <= WATCH_PCT {
                                statuses.push(Status::Watch);
                            }
                            trade = Some(found);
                        }
                        Err(reason) => skipped = Some(reason),
                    }
                }
            },
        }
        if flip
            .as_ref()
            .is_some_and(|flip| flip.distance_pct <= FLIP_PCT)
        {
            statuses.push(Status::FlipWatch);
        }
        if counter_trend.is_some() {
            statuses.push(Status::CounterTrend);
        }
        Setup {
            bias: match bias {
                1 => "bullish",
                -1 => "bearish",
                _ => "none",
            },
            statuses,
            primary,
            trade,
            skipped,
            flip,
            counter_trend,
            tap_ago,
            mss_ago,
            ran,
        }
    }
}

/// LIVE setups in the order to read them: closest to the entry first, then
/// the best R to T2, then the higher max leverage (the bigger coins).
pub fn rank_live(setups: &mut [(String, Trade)]) {
    setups.sort_by(|(_, a), (_, b)| {
        a.entry_distance_pct
            .total_cmp(&b.entry_distance_pct)
            .then_with(|| {
                b.t2_r
                    .unwrap_or(f64::MIN)
                    .total_cmp(&a.t2_r.unwrap_or(f64::MIN))
            })
            .then_with(|| b.max_leverage.total_cmp(&a.max_leverage))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: Rules = Rules {
        max_stop_pct: 5.0,
        leverage: 5.0,
        risk_pct: 0.5,
        counter_trend_risk_pct: 0.25,
    };

    /// A bearish coin at 100 with a resistance order block 102-104 (bear
    /// slot s1), fib 103, daily ATR 2, 24h low 98, 1M low 90.
    fn bearish() -> CoinInput {
        let mut input = CoinInput {
            close: 100.0,
            atr: Some(2.0),
            high_24h: Some(101.0),
            low_24h: Some(98.0),
            high_1m: Some(110.0),
            low_1m: Some(90.0),
            values: BTreeMap::new(),
        };
        for (name, value) in [
            ("x_bias", -1.0),
            ("x_s1Top", 104.0),
            ("x_s1Bot", 102.0),
            ("x_s1St", 0.0),
            ("x_fib50", 103.0),
            ("x_bullFlip", 106.0),
            ("x_tapDnAgo", 30.0),
            ("x_mssDir", -1.0),
            ("x_mssAgo", 5.0),
            ("x_ctArm", 0.0),
        ] {
            input.values.insert(name.to_owned(), Some(value));
        }
        input
    }

    fn set(input: &mut CoinInput, name: &str, value: Option<f64>) {
        input.values.insert(name.to_owned(), value);
    }

    fn near(left: f64, right: f64) -> bool {
        (left - right).abs() < 1e-9
    }

    #[test]
    fn zone_sides_follow_slot_and_state() {
        let mut input = bearish();
        // A bull breaker above price resists too; a bull order block below
        // supports; a bear breaker below supports.
        for (name, value) in [
            ("x_b1Top", 108.0),
            ("x_b1Bot", 107.0),
            ("x_b1St", 1.0),
            ("x_b2Top", 97.0),
            ("x_b2Bot", 96.0),
            ("x_b2St", 0.0),
            ("x_s2Top", 95.0),
            ("x_s2Bot", 94.0),
            ("x_s2St", 3.0), // breaker, hidden
        ] {
            set(&mut input, name, Some(value));
        }
        let shorts = input.levels(Direction::Short);
        assert_eq!(
            shorts.iter().map(|l| l.slot.unwrap()).collect::<Vec<_>>(),
            ["s1", "b1"]
        );
        assert!(near(shorts[0].distance_pct, 2.0));
        assert_eq!(shorts[1].kind, LevelKind::Breaker);
        let longs = input.levels(Direction::Long);
        assert_eq!(
            longs.iter().map(|l| l.slot.unwrap()).collect::<Vec<_>>(),
            ["b2", "s2"]
        );
        assert!(longs[1].hidden);
        assert_eq!(longs[1].kind, LevelKind::Breaker);
    }

    #[test]
    fn the_nearer_of_zone_and_broken_level_is_primary() {
        let mut input = bearish();
        set(&mut input, "x_brkLvl", Some(101.0));
        let levels = input.levels(Direction::Short);
        assert_eq!(levels[0].kind, LevelKind::BrokenLevel);
        assert!(near(levels[0].distance_pct, 1.0));
        // A broken level below price does not back a short.
        set(&mut input, "x_brkLvl", Some(99.0));
        assert_eq!(input.levels(Direction::Short)[0].slot, Some("s1"));
    }

    #[test]
    fn entry_stop_targets_and_size() {
        let input = bearish();
        let level = input.levels(Direction::Short)[0];
        let trade = input.trade(Direction::Short, &level, &RULES).unwrap();
        // Entry: the near edge 102 (the fib at 103 is farther).
        assert!(near(trade.entry, 102.0));
        // Stop: beyond the far edge 104 (the fib inside the zone too) + 0.3 ATR.
        assert!(near(trade.stop, 104.6));
        assert!(near(trade.stop_pct, 2.6 / 102.0 * 100.0));
        // T1 24h low 98: 4 / 2.6; T2 1M low 90: 12 / 2.6.
        assert!(near(trade.t1_r.unwrap(), 4.0 / 2.6));
        assert!(near(trade.t2_r.unwrap(), 12.0 / 2.6));
        assert!(near(trade.notional_per_10, 10.0 / (2.6 / 102.0)));
        assert!(near(trade.margin_per_10, trade.notional_per_10 / 5.0));
    }

    #[test]
    fn a_closer_fib_is_the_entry_and_part_of_the_area() {
        let mut input = bearish();
        set(&mut input, "x_fib50", Some(101.0));
        let level = input.levels(Direction::Short)[0];
        let trade = input.trade(Direction::Short, &level, &RULES).unwrap();
        assert!(near(trade.entry, 101.0));
        assert!(near(trade.stop, 104.6));
    }

    #[test]
    fn inside_the_zone_the_entry_is_the_close() {
        let mut input = bearish();
        input.close = 103.0;
        let level = input.levels(Direction::Short)[0];
        assert_eq!(level.distance_pct, 0.0);
        let trade = input.trade(Direction::Short, &level, &RULES).unwrap();
        assert!(near(trade.entry, 103.0));
    }

    #[test]
    fn a_wide_stop_is_skipped() {
        let mut input = bearish();
        input.atr = Some(20.0);
        let setup = input.setup(&RULES);
        assert!(setup.trade.is_none());
        assert!(setup.skipped.unwrap().contains("over the plan's max 5%"));
        assert!(!setup.statuses.contains(&Status::Watch));
    }

    #[test]
    fn watch_within_five_percent() {
        let setup = bearish().setup(&RULES);
        assert_eq!(setup.statuses, [Status::Watch]);
        let mut far = bearish();
        set(&mut far, "x_s1Bot", Some(106.0));
        set(&mut far, "x_s1Top", Some(107.0));
        set(&mut far, "x_fib50", Some(106.5));
        set(&mut far, "x_bullFlip", Some(120.0));
        assert!(far.setup(&RULES).statuses.is_empty());
    }

    #[test]
    fn live_needs_a_recent_tap_and_a_later_aligned_mss() {
        let mut input = bearish();
        set(&mut input, "x_tapDnAgo", Some(10.0));
        assert_eq!(input.setup(&RULES).statuses, [Status::Live]);
        // The MSS came before the tap.
        set(&mut input, "x_mssAgo", Some(12.0));
        assert_eq!(input.setup(&RULES).statuses, [Status::Watch]);
        // The MSS was the other way.
        set(&mut input, "x_mssAgo", Some(5.0));
        set(&mut input, "x_mssDir", Some(1.0));
        assert_eq!(input.setup(&RULES).statuses, [Status::Watch]);
        // The tap is older than 24 bars.
        set(&mut input, "x_mssDir", Some(-1.0));
        set(&mut input, "x_tapDnAgo", Some(25.0));
        assert_eq!(input.setup(&RULES).statuses, [Status::Watch]);
    }

    #[test]
    fn a_triggered_setup_that_already_ran_is_ran_not_live() {
        let mut input = bearish();
        set(&mut input, "x_tapDnAgo", Some(10.0));
        // Entry 102, stop 104.6: 2.6 of risk; price 2 below the entry.
        let setup = input.setup(&RULES);
        assert_eq!(setup.statuses, [Status::Live]);
        assert_eq!(setup.ran, None);
        // 3 below the entry: further than the stop distance.
        input.close = 99.0;
        let setup = input.setup(&RULES);
        assert_eq!(setup.statuses, [Status::Ran]);
        assert!(setup.ran.unwrap().contains("past the entry"));
        // At T1 (the 24h low) is ran too.
        input.close = 100.0;
        input.low_24h = Some(100.0);
        let setup = input.setup(&RULES);
        assert_eq!(setup.statuses, [Status::Ran]);
        assert!(setup.ran.unwrap().contains("T1"));
        // A WATCH below its short level is waiting for the pullback, not ran.
        let mut watch = bearish();
        watch.close = 99.0;
        assert_eq!(watch.setup(&RULES).statuses, [Status::Watch]);
    }

    #[test]
    fn levels_say_what_they_are() {
        let mut input = bearish();
        set(&mut input, "x_s2Top", Some(97.0));
        set(&mut input, "x_s2Bot", Some(96.0));
        set(&mut input, "x_s2St", Some(1.0));
        let short = input.levels(Direction::Short)[0];
        assert_eq!((short.name, short.role), ("bear order block", "resistance"));
        let long = input.levels(Direction::Long)[0];
        assert_eq!(long.slot, Some("s2"));
        assert_eq!(
            (long.name, long.role),
            ("bear-slot breaker (broken bear OB)", "support")
        );
    }

    #[test]
    fn live_setups_rank_by_entry_distance_then_t2_r_then_leverage() {
        let trade = |distance: f64, t2_r: Option<f64>, leverage: f64| Trade {
            direction: Direction::Short,
            entry: 100.0,
            entry_distance_pct: distance,
            stop: 101.0,
            stop_pct: 1.0,
            t1: None,
            t1_r: None,
            t2: None,
            t2_r,
            notional_per_10: 1000.0,
            margin_per_10: 100.0,
            max_leverage: leverage,
            risk_pct: 0.5,
        };
        let mut live = vec![
            ("FAR".to_owned(), trade(2.0, Some(5.0), 10.0)),
            ("LOW-R".to_owned(), trade(0.5, Some(1.0), 10.0)),
            ("SMALL".to_owned(), trade(0.5, Some(3.0), 3.0)),
            ("BIG".to_owned(), trade(0.5, Some(3.0), 10.0)),
            ("NO-T2".to_owned(), trade(0.5, None, 10.0)),
        ];
        rank_live(&mut live);
        let order: Vec<&str> = live.iter().map(|(coin, _)| coin.as_str()).collect();
        assert_eq!(order, ["BIG", "SMALL", "LOW-R", "NO-T2", "FAR"]);
    }

    #[test]
    fn flip_watch_within_three_percent() {
        let mut input = bearish();
        let flip = input.flip(-1).unwrap();
        assert_eq!(flip.to, "bullish");
        assert!(near(flip.distance_pct, 6.0));
        assert!(!input.setup(&RULES).statuses.contains(&Status::FlipWatch));
        set(&mut input, "x_bullFlip", Some(102.5));
        assert!(input.setup(&RULES).statuses.contains(&Status::FlipWatch));
        // A bullish bias flips on x_bearFlip.
        set(&mut input, "x_bearFlip", Some(97.0));
        assert_eq!(input.flip(1).unwrap().to, "bearish");
        assert_eq!(input.flip(0), None);
    }

    #[test]
    fn counter_trend_long_after_an_sfp_against_the_bias() {
        let mut input = bearish();
        set(&mut input, "x_ctArm", Some(1.0));
        set(&mut input, "x_ctWick", Some(97.0));
        let ct = input.counter_trend(&RULES).unwrap();
        assert_eq!(ct.direction, Direction::Long);
        // Stop below the wick by 0.2 ATR; target the nearest resistance (102).
        assert!(near(ct.stop, 96.6));
        assert_eq!(ct.t1, Some(102.0));
        assert!(near(ct.t1_r.unwrap(), 2.0 / 3.4));
        assert_eq!(ct.risk_pct, 0.25);
        // The broken level is the target when there is one above price.
        set(&mut input, "x_brkLvl", Some(101.0));
        assert_eq!(input.counter_trend(&RULES).unwrap().t1, Some(101.0));
        let setup = input.setup(&RULES);
        assert!(setup.statuses.contains(&Status::CounterTrend));
        // Not armed: none.
        set(&mut input, "x_ctArm", Some(0.0));
        assert!(input.counter_trend(&RULES).is_none());
    }

    #[test]
    fn no_bias_or_no_level_says_why() {
        let mut input = bearish();
        set(&mut input, "x_bias", Some(0.0));
        assert_eq!(
            input.setup(&RULES).skipped.as_deref(),
            Some("no HTF bias yet")
        );
        let mut input = bearish();
        set(&mut input, "x_s1Top", None);
        assert!(
            input
                .setup(&RULES)
                .skipped
                .unwrap()
                .contains("no zone or broken level")
        );
    }
}
