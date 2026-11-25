//! Indicator math for the market snapshot, the same as TradingView's
//! built-ins: Wilder's RSI and ATR (RMA smoothing) and the simple average.
//! All take values oldest first and return the value at the newest one, or
//! `None` without enough history.

/// Wilder's moving average (TradingView `ta.rma`): seeded with the simple
/// average of the first `length` values.
fn rma(values: &[f64], length: usize) -> Option<f64> {
    if length == 0 || values.len() < length {
        return None;
    }
    let mut average = values[..length].iter().sum::<f64>() / length as f64;
    for value in &values[length..] {
        average = (average * (length - 1) as f64 + value) / length as f64;
    }
    Some(average)
}

/// `ta.rsi(close, length)`.
pub fn rsi(closes: &[f64], length: usize) -> Option<f64> {
    if closes.len() <= length {
        return None;
    }
    let (gains, losses): (Vec<f64>, Vec<f64>) = closes
        .windows(2)
        .map(|pair| {
            let change = pair[1] - pair[0];
            (change.max(0.0), (-change).max(0.0))
        })
        .unzip();
    let gain = rma(&gains, length)?;
    let loss = rma(&losses, length)?;
    Some(if loss == 0.0 {
        100.0
    } else if gain == 0.0 {
        0.0
    } else {
        100.0 - 100.0 / (1.0 + gain / loss)
    })
}

/// `ta.atr(length)` from `(high, low, close)` bars.
pub fn atr(bars: &[(f64, f64, f64)], length: usize) -> Option<f64> {
    if bars.is_empty() {
        return None;
    }
    // The first bar has no previous close: its range.
    let mut ranges = vec![bars[0].0 - bars[0].1];
    for pair in bars.windows(2) {
        let previous_close = pair[0].2;
        let (high, low, _) = pair[1];
        ranges.push(
            (high - low)
                .max((high - previous_close).abs())
                .max((low - previous_close).abs()),
        );
    }
    rma(&ranges, length)
}

/// `ta.sma(close, length)`.
pub fn sma(closes: &[f64], length: usize) -> Option<f64> {
    if length == 0 || closes.len() < length {
        return None;
    }
    Some(closes[closes.len() - length..].iter().sum::<f64>() / length as f64)
}

/// Percent change from `from` to `to`.
pub fn pct(from: f64, to: f64) -> Option<f64> {
    (from != 0.0 && from.is_finite() && to.is_finite()).then(|| (to - from) / from * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: Option<f64>, right: f64) -> bool {
        left.is_some_and(|left| (left - right).abs() < 1e-9)
    }

    #[test]
    fn rma_seeds_with_the_simple_average() {
        assert!(close(rma(&[1.0, 2.0, 3.0], 3), 2.0));
        // (2 * 2 + 6) / 3
        assert!(close(rma(&[1.0, 2.0, 3.0, 6.0], 3), 10.0 / 3.0));
        assert_eq!(rma(&[1.0, 2.0], 3), None);
    }

    #[test]
    fn rsi_follows_wilder() {
        // Changes +1 +1 -1 +2: gains 1 1 0 2, losses 0 0 1 0.
        // length 3: gain rma = (2/3 * 2 + 2) / 3 = 10/9, loss = (1/3 * 2) / 3 = 2/9
        let value = rsi(&[10.0, 11.0, 12.0, 11.0, 13.0], 3);
        assert!(close(value, 100.0 - 100.0 / (1.0 + 5.0)));
        assert!(close(rsi(&[1.0, 2.0, 3.0, 4.0], 3), 100.0));
        assert!(close(rsi(&[4.0, 3.0, 2.0, 1.0], 3), 0.0));
        assert_eq!(rsi(&[1.0, 2.0, 3.0], 3), None);
    }

    #[test]
    fn atr_uses_the_true_range() {
        // Ranges: 2 (first bar), max(1, |12-9|, |11-9|) = 3, max(4, |14-12|, |10-12|) = 4.
        let bars = [(10.0, 8.0, 9.0), (12.0, 11.0, 12.0), (14.0, 10.0, 13.0)];
        assert!(close(atr(&bars, 3), 3.0));
        assert!(close(atr(&bars, 2), (2.5 + 4.0) / 2.0));
        assert_eq!(atr(&bars, 4), None);
    }

    #[test]
    fn sma_and_pct() {
        assert!(close(sma(&[1.0, 2.0, 3.0, 4.0], 2), 3.5));
        assert_eq!(sma(&[1.0], 2), None);
        assert!(close(pct(50.0, 55.0), 10.0));
        assert_eq!(pct(0.0, 5.0), None);
    }
}
