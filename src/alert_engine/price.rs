//! Price alerts: does a price move fire the alert?
//!
//! A cross needs a price on each side of the level: the previous trade
//! below (above) and this one at or above (at or below) it. A price that is
//! already past the level when the alert starts does not fire until it comes
//! back and crosses again.
//!
//! `once` alerts fire on the cross. `every_bar` alerts fire on the cross and
//! then once per bar of the alert's timeframe while the price stays past the
//! level; moving back resets them until the next cross.

use data::config::alert_store::{Frequency, PriceCross};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
}

/// What a price move did to one alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fire {
    /// Crossed the level this way.
    Crossed(Side),
    /// `every_bar`: still past the level on a new bar.
    Stays(Side),
}

/// One alert's memory between trades.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PriceState {
    /// `every_bar`: the side it crossed to, while the price stays there.
    across: Option<Side>,
    /// Open time of the bar it last fired in.
    fired_bar: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceRule {
    pub cross: PriceCross,
    pub level: f64,
    pub frequency: Frequency,
}

impl PriceRule {
    /// The price moved from `previous` to `price` in the bar opened at `bar`.
    pub fn step(
        &self,
        state: &mut PriceState,
        previous: f64,
        price: f64,
        bar: u64,
    ) -> Option<Fire> {
        let level = self.level;
        let up = previous < level && price >= level;
        let down = previous > level && price <= level;
        let crossed = match self.cross {
            PriceCross::Above => up.then_some(Side::Above),
            PriceCross::Below => down.then_some(Side::Below),
            PriceCross::Cross => {
                if up {
                    Some(Side::Above)
                } else if down {
                    Some(Side::Below)
                } else {
                    None
                }
            }
        };
        let every_bar = self.frequency == Frequency::EveryBar;

        if let Some(side) = crossed {
            if every_bar {
                state.across = Some(side);
                if state.fired_bar == Some(bar) {
                    return None;
                }
            }
            state.fired_bar = Some(bar);
            return Some(Fire::Crossed(side));
        }

        let side = state.across?;
        let stays = match side {
            Side::Above => price >= level,
            Side::Below => price <= level,
        };
        if !stays {
            state.across = None;
            return None;
        }
        if state.fired_bar == Some(bar) {
            return None;
        }
        state.fired_bar = Some(bar);
        Some(Fire::Stays(side))
    }

    /// A candle seen instead of its trades (after a reconnect): the price went
    /// from `previous` through the candle's high and low to its close. The
    /// extreme nearer the level's side is visited first, so a cross is never
    /// missed.
    pub fn step_candle(
        &self,
        state: &mut PriceState,
        previous: f64,
        (high, low, close): (f64, f64, f64),
        bar: u64,
    ) -> Vec<Fire> {
        let path = if previous < self.level {
            [high, low, close]
        } else {
            [low, high, close]
        };
        let mut fired = Vec::new();
        let mut last = previous;
        for price in path {
            if let Some(fire) = self.step(state, last, price, bar) {
                fired.push(fire);
            }
            last = price;
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = 60_000;

    fn rule(cross: PriceCross, frequency: Frequency) -> PriceRule {
        PriceRule {
            cross,
            level: 100.0,
            frequency,
        }
    }

    /// Feeds `(time, price)` trades and returns what fired, with the time.
    fn run(rule: PriceRule, bar_ms: u64, trades: &[(u64, f64)]) -> Vec<(u64, Fire)> {
        let mut state = PriceState::default();
        let mut fired = Vec::new();
        for pair in trades.windows(2) {
            let ((_, previous), (time, price)) = (pair[0], pair[1]);
            if let Some(fire) = rule.step(&mut state, previous, price, time / bar_ms * bar_ms) {
                fired.push((time, fire));
            }
        }
        fired
    }

    /// Recorded-style trades around a level of 100: up through it, a dip,
    /// back up, then a long stay above and a drop.
    const TRADES: &[(u64, f64)] = &[
        (0, 99.5),
        (10_000, 99.9),
        (20_000, 100.0), // touches the level from below: crosses up
        (30_000, 100.4),
        (40_000, 99.8),   // back below: crosses down
        (50_000, 100.2),  // up again, same minute
        (70_000, 100.6),  // next minute, still above
        (130_000, 101.0), // third minute, still above
        (140_000, 101.2),
        (190_000, 99.0), // fourth minute, drops below
        (250_000, 99.2),
    ];

    #[test]
    fn once_fires_on_each_cross_its_way() {
        assert_eq!(
            run(rule(PriceCross::Above, Frequency::Once), MINUTE, TRADES),
            [
                (20_000, Fire::Crossed(Side::Above)),
                (50_000, Fire::Crossed(Side::Above))
            ]
        );
        assert_eq!(
            run(rule(PriceCross::Below, Frequency::Once), MINUTE, TRADES),
            [
                (40_000, Fire::Crossed(Side::Below)),
                (190_000, Fire::Crossed(Side::Below))
            ]
        );
        assert_eq!(
            run(rule(PriceCross::Cross, Frequency::Once), MINUTE, TRADES).len(),
            4
        );
    }

    #[test]
    fn every_bar_fires_once_per_bar_while_across() {
        assert_eq!(
            run(rule(PriceCross::Above, Frequency::EveryBar), MINUTE, TRADES),
            [
                // First minute: the cross; the second cross in it is skipped.
                (20_000, Fire::Crossed(Side::Above)),
                // Still above in the second and third minutes.
                (70_000, Fire::Stays(Side::Above)),
                (130_000, Fire::Stays(Side::Above)),
            ]
        );
        // Either way: after the drop it stays below in the fifth minute.
        let fired = run(rule(PriceCross::Cross, Frequency::EveryBar), MINUTE, TRADES);
        assert_eq!(fired.last(), Some(&(250_000, Fire::Stays(Side::Below))));
    }

    #[test]
    fn already_past_the_level_waits_for_a_cross() {
        let trades = [(0, 101.0), (60_000, 102.0), (120_000, 103.0)];
        assert!(
            run(
                rule(PriceCross::Above, Frequency::EveryBar),
                MINUTE,
                &trades
            )
            .is_empty()
        );
    }

    #[test]
    fn a_candle_after_a_reconnect_catches_a_cross() {
        let rule = rule(PriceCross::Above, Frequency::Once);
        let mut state = PriceState::default();
        // Last trade 99, then one candle that went to 100.5 and closed at 99.7.
        let fired = rule.step_candle(&mut state, 99.0, (100.5, 98.0, 99.7), 0);
        assert_eq!(fired, [Fire::Crossed(Side::Above)]);
        // A candle that stayed below does not fire.
        let rule = PriceRule {
            level: 200.0,
            ..rule
        };
        assert!(
            rule.step_candle(&mut PriceState::default(), 99.0, (100.5, 98.0, 99.7), 0)
                .is_empty()
        );
    }
}
