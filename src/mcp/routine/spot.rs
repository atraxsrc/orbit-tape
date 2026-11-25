//! The daily routine's spot accumulation check: where price is against the
//! plan's reference price and levels, the monthly buys done and next, the
//! lump-sum parts left, and SPOT ZONE days. All amounts and levels come from
//! the plan; nothing here buys anything.

use chrono::NaiveDate;
use data::config::plan::{LevelKind, Spot, first_monday_after_month_of};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default)]
pub struct Market {
    pub close: f64,
    pub rsi_1d: Option<f64>,
    pub rsi_1w: Option<f64>,
    pub sma200_1w: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NextLevel {
    pub price: f64,
    pub label: String,
    pub kind: &'static str,
    /// From the close down to the level, % (negative).
    pub distance_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Part {
    pub label: String,
    pub price: f64,
    pub amount: f64,
    pub used: bool,
    pub distance_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Monthly {
    pub amount: f64,
    pub planned: usize,
    pub done: usize,
    pub skipped: usize,
    /// The next planned buy, today included.
    pub next: Option<NaiveDate>,
    pub today: bool,
    /// After the last planned date: unused lump-sum parts spread over
    /// first-Monday buys.
    pub spread_next: Option<NaiveDate>,
    pub spread_buys_left: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub pair: String,
    pub close: f64,
    pub holdings: f64,
    pub goal: Option<f64>,
    pub stretch_goal: Option<f64>,
    pub reference_price: Option<f64>,
    pub reference_note: String,
    pub vs_reference_pct: Option<f64>,
    pub next_level_below: Option<NextLevel>,
    pub rsi_1d: Option<f64>,
    pub rsi_1w: Option<f64>,
    pub sma200_1w: Option<f64>,
    pub vs_sma200_1w_pct: Option<f64>,
    pub monthly: Option<Monthly>,
    pub parts: Vec<Part>,
    pub parts_left_amount: f64,
    pub spot_zone: bool,
    /// Why it is a SPOT ZONE, and what the plan says to do.
    pub zone_reasons: Vec<String>,
}

fn signed_pct(from: f64, to: f64) -> f64 {
    (to - from) / from * 100.0
}

/// The spot status on `today` (the user's local date).
pub fn status(spot: &Spot, market: Market, today: NaiveDate) -> Status {
    let close = market.close;
    let mut zone_reasons = Vec::new();

    let parts: Vec<Part> = spot
        .levels
        .iter()
        .filter(|level| level.kind == LevelKind::Buy)
        .map(|level| Part {
            label: level.label.clone(),
            price: level.price,
            amount: level.amount.unwrap_or_default(),
            used: level.used,
            distance_pct: signed_pct(close, level.price),
        })
        .collect();
    let parts_left_amount: f64 = parts.iter().filter(|p| !p.used).map(|p| p.amount).sum();
    for part in parts.iter().filter(|part| !part.used) {
        let near = (close - part.price).abs() / part.price * 100.0 <= spot.zone_pct;
        if near || close <= part.price {
            zone_reasons.push(format!(
                "price {close} is {} {} at {} (unused): get ready, redeem {} from earn",
                if close <= part.price {
                    "at or below".to_owned()
                } else {
                    format!("within {}% of", spot.zone_pct)
                },
                part.label,
                part.price,
                part.amount
            ));
        }
    }

    // Levels still waiting below price: heads-ups, and buy parts not used.
    let next_level_below = spot
        .levels
        .iter()
        .filter(|level| level.price < close)
        .filter(|level| level.kind == LevelKind::HeadsUp || !level.used)
        .max_by(|a, b| a.price.total_cmp(&b.price))
        .map(|level| NextLevel {
            price: level.price,
            label: level.label.clone(),
            kind: match level.kind {
                LevelKind::HeadsUp => "heads_up",
                LevelKind::Buy => "buy",
            },
            distance_pct: signed_pct(close, level.price),
        });

    let monthly = spot.monthly.as_ref().map(|monthly| {
        let skipped = |date: &NaiveDate| monthly.skipped.contains(date);
        let done = monthly
            .dates
            .iter()
            .filter(|date| **date < today && !skipped(date))
            .count();
        let next = monthly
            .dates
            .iter()
            .find(|date| **date >= today && !skipped(date))
            .copied();
        let today_planned = next == Some(today);
        let last = monthly.dates.last().copied();
        let (spread_next, spread_buys_left) = match last {
            Some(last) if today > last && monthly.spread_leftover && parts_left_amount > 0.0 => {
                let mut date = first_monday_after_month_of(last);
                while date < today {
                    date = first_monday_after_month_of(date);
                }
                (
                    Some(date),
                    Some((parts_left_amount / monthly.amount).ceil() as u32),
                )
            }
            _ => (None, None),
        };
        if today_planned || spread_next == Some(today) {
            zone_reasons.push(format!("monthly buy today: {}", monthly.amount));
        }
        Monthly {
            amount: monthly.amount,
            planned: monthly.dates.len(),
            done,
            skipped: monthly.skipped.len(),
            next,
            today: today_planned || spread_next == Some(today),
            spread_next,
            spread_buys_left,
        }
    });

    Status {
        pair: spot.symbol.clone(),
        close,
        holdings: spot.holdings,
        goal: spot.goal,
        stretch_goal: spot.stretch_goal,
        reference_price: spot.realized_price,
        reference_note: spot.realized_price_note.clone(),
        vs_reference_pct: spot.realized_price.map(|price| signed_pct(price, close)),
        next_level_below,
        rsi_1d: market.rsi_1d,
        rsi_1w: market.rsi_1w,
        sma200_1w: market.sma200_1w,
        vs_sma200_1w_pct: market.sma200_1w.map(|sma| signed_pct(sma, close)),
        monthly,
        parts,
        parts_left_amount,
        spot_zone: !zone_reasons.is_empty(),
        zone_reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::config::plan::Plan;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The example plan: levels 56000 (heads-up), 50000 and 45000 (buys of
    /// 2500), monthly 800 on 2027-07-05, 08-02, 09-06, 10-04.
    fn spot() -> Spot {
        Plan::parse(include_str!("../../../docs/plan.example.toml"))
            .unwrap()
            .spot
            .unwrap()
    }

    fn market(close: f64) -> Market {
        Market {
            close,
            rsi_1d: Some(45.0),
            rsi_1w: Some(40.0),
            sma200_1w: Some(40000.0),
        }
    }

    #[test]
    fn a_quiet_day() {
        let status = status(&spot(), market(60000.0), date(2027, 7, 20));
        assert!(!status.spot_zone, "{:?}", status.zone_reasons);
        assert!((status.vs_reference_pct.unwrap() - 20.0).abs() < 1e-9);
        assert!((status.vs_sma200_1w_pct.unwrap() - 50.0).abs() < 1e-9);
        let next = status.next_level_below.unwrap();
        assert_eq!((next.price, next.kind), (56000.0, "heads_up"));
        let monthly = status.monthly.unwrap();
        assert_eq!((monthly.done, monthly.planned), (1, 4));
        assert_eq!(monthly.next, Some(date(2027, 8, 2)));
        assert!(!monthly.today);
        assert_eq!(status.parts_left_amount, 5000.0);
    }

    #[test]
    fn a_monthly_buy_day_is_a_spot_zone() {
        let status = status(&spot(), market(60000.0), date(2027, 8, 2));
        assert!(status.spot_zone);
        assert!(status.monthly.as_ref().unwrap().today);
        assert!(status.zone_reasons[0].starts_with("monthly buy today: 800"));
        assert_eq!(status.monthly.unwrap().done, 1, "today is not done yet");
    }

    #[test]
    fn near_an_unused_buy_level_is_a_spot_zone() {
        // 51000 is 2% above 50000.
        let status = status(&spot(), market(51000.0), date(2027, 7, 20));
        assert!(status.spot_zone);
        assert!(status.zone_reasons[0].contains("BUY 1"));
        // Used: no zone, and the next level below is BUY 2.
        let mut used = spot();
        used.levels
            .iter_mut()
            .find(|l| l.price == 50000.0)
            .unwrap()
            .used = true;
        let status = super::status(&used, market(51000.0), date(2027, 7, 20));
        assert!(!status.spot_zone);
        assert_eq!(status.next_level_below.unwrap().price, 45000.0);
        assert_eq!(status.parts_left_amount, 2500.0);
    }

    #[test]
    fn skipped_dates_are_not_done() {
        let mut spot = spot();
        spot.monthly.as_mut().unwrap().skipped = vec![date(2027, 7, 5)];
        let monthly = status(&spot, market(60000.0), date(2027, 8, 20))
            .monthly
            .unwrap();
        assert_eq!((monthly.done, monthly.skipped), (1, 1));
        assert_eq!(monthly.next, Some(date(2027, 9, 6)));
    }

    #[test]
    fn leftover_parts_spread_over_first_mondays_after_the_last_date() {
        // Last planned date 2027-10-04; both parts unused: 5000 / 800 = 7 buys.
        let status = status(&spot(), market(60000.0), date(2027, 10, 20));
        let monthly = status.monthly.unwrap();
        assert_eq!(monthly.next, None);
        assert_eq!(monthly.spread_next, Some(date(2027, 11, 1)));
        assert_eq!(monthly.spread_buys_left, Some(7));
        // On that Monday it is a buy day.
        let status = super::status(&spot(), market(60000.0), date(2027, 11, 1));
        assert!(status.spot_zone);
        assert!(status.monthly.unwrap().today);
    }
}
