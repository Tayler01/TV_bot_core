//! Built-in, strategy-agnostic indicator primitives.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use thiserror::Error;
use tv_bot_core_types::{MarketEvent, Timeframe};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Debug, PartialEq)]
pub struct BarInput {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: u64,
    pub closed_at: DateTime<Utc>,
}

impl TryFrom<&MarketEvent> for BarInput {
    type Error = IndicatorError;

    fn try_from(event: &MarketEvent) -> Result<Self, Self::Error> {
        match event {
            MarketEvent::Bar {
                symbol,
                timeframe,
                open,
                high,
                low,
                close,
                volume,
                closed_at,
            } => Ok(Self {
                symbol: symbol.clone(),
                timeframe: *timeframe,
                open: *open,
                high: *high,
                low: *low,
                close: *close,
                volume: *volume,
                closed_at: *closed_at,
            }),
            _ => Err(IndicatorError::NotBarEvent),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IndicatorError {
    #[error("indicator series is empty")]
    EmptySeries,
    #[error("indicator requires at least {needed} bars but only {available} are available")]
    InsufficientBars { needed: usize, available: usize },
    #[error("market event is not a bar event")]
    NotBarEvent,
    #[error("bar series contains mismatched timeframes: expected {expected:?}, found {found:?}")]
    TimeframeMismatch {
        expected: Timeframe,
        found: Timeframe,
    },
    #[error("indicator period must be greater than zero")]
    InvalidPeriod,
}

pub fn bars_from_events<'a, I>(
    events: I,
    expected_timeframe: Timeframe,
) -> Result<Vec<BarInput>, IndicatorError>
where
    I: IntoIterator<Item = &'a MarketEvent>,
{
    let mut bars = Vec::new();

    for event in events {
        let bar = BarInput::try_from(event)?;
        if bar.timeframe != expected_timeframe {
            return Err(IndicatorError::TimeframeMismatch {
                expected: expected_timeframe,
                found: bar.timeframe,
            });
        }
        bars.push(bar);
    }

    Ok(bars)
}

pub fn latest_bar(bars: &[BarInput]) -> Result<&BarInput, IndicatorError> {
    validate_series(bars, 1)?;
    Ok(bars.last().expect("validated non-empty bars"))
}

pub fn simple_moving_average(bars: &[BarInput], period: usize) -> Result<Decimal, IndicatorError> {
    validate_series(bars, period)?;
    let tail = &bars[bars.len() - period..];
    let sum = tail.iter().fold(Decimal::ZERO, |acc, bar| acc + bar.close);
    Ok(sum / Decimal::from(period as u64))
}

pub fn exponential_moving_average(
    bars: &[BarInput],
    period: usize,
) -> Result<Decimal, IndicatorError> {
    validate_series(bars, period)?;

    let seed = simple_moving_average(&bars[..period], period)?;
    let multiplier = Decimal::from(2u32) / Decimal::from((period + 1) as u64);

    let ema = bars[period..]
        .iter()
        .fold(seed, |acc, bar| ((bar.close - acc) * multiplier) + acc);

    Ok(ema)
}

pub fn highest_high(bars: &[BarInput], lookback: usize) -> Result<Decimal, IndicatorError> {
    validate_series(bars, lookback)?;
    Ok(bars[bars.len() - lookback..]
        .iter()
        .map(|bar| bar.high)
        .max()
        .expect("validated non-empty bars"))
}

pub fn lowest_low(bars: &[BarInput], lookback: usize) -> Result<Decimal, IndicatorError> {
    validate_series(bars, lookback)?;
    Ok(bars[bars.len() - lookback..]
        .iter()
        .map(|bar| bar.low)
        .min()
        .expect("validated non-empty bars"))
}

pub fn average_volume(bars: &[BarInput], period: usize) -> Result<Decimal, IndicatorError> {
    validate_series(bars, period)?;
    let tail = &bars[bars.len() - period..];
    let volume_sum = tail
        .iter()
        .fold(Decimal::ZERO, |acc, bar| acc + Decimal::from(bar.volume));
    Ok(volume_sum / Decimal::from(period as u64))
}

pub fn bar_range(bar: &BarInput) -> Decimal {
    bar.high - bar.low
}

pub fn bar_body(bar: &BarInput) -> Decimal {
    if bar.close >= bar.open {
        bar.close - bar.open
    } else {
        bar.open - bar.close
    }
}

pub fn upper_wick(bar: &BarInput) -> Decimal {
    let body_top = if bar.close >= bar.open {
        bar.close
    } else {
        bar.open
    };
    bar.high - body_top
}

pub fn lower_wick(bar: &BarInput) -> Decimal {
    let body_bottom = if bar.close <= bar.open {
        bar.close
    } else {
        bar.open
    };
    body_bottom - bar.low
}

pub fn close_position_ratio(bar: &BarInput) -> Decimal {
    let range = bar_range(bar);
    if range.is_zero() {
        Decimal::new(5, 1)
    } else {
        (bar.close - bar.low) / range
    }
}

fn validate_series(bars: &[BarInput], required_bars: usize) -> Result<(), IndicatorError> {
    if required_bars == 0 {
        return Err(IndicatorError::InvalidPeriod);
    }

    let first = bars.first().ok_or(IndicatorError::EmptySeries)?;

    if bars.len() < required_bars {
        return Err(IndicatorError::InsufficientBars {
            needed: required_bars,
            available: bars.len(),
        });
    }

    for bar in bars.iter().skip(1) {
        if bar.timeframe != first.timeframe {
            return Err(IndicatorError::TimeframeMismatch {
                expected: first.timeframe,
                found: bar.timeframe,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn bar(index: i64, close: i64, high: i64, low: i64, volume: u64) -> BarInput {
        let closed_at = Utc::now() + Duration::minutes(index);
        BarInput {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(close - 1),
            high: Decimal::from(high),
            low: Decimal::from(low),
            close: Decimal::from(close),
            volume,
            closed_at,
        }
    }

    #[test]
    fn converts_bar_events_into_indicator_inputs() {
        let event = MarketEvent::Bar {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(10),
            high: Decimal::from(12),
            low: Decimal::from(9),
            close: Decimal::from(11),
            volume: 50,
            closed_at: Utc::now(),
        };

        let bar = BarInput::try_from(&event).expect("bar conversion should work");
        assert_eq!(bar.symbol, "GCM2026");
        assert_eq!(bar.timeframe, Timeframe::OneMinute);
        assert_eq!(bar.close, Decimal::from(11));
    }

    #[test]
    fn moving_averages_and_volume_average_use_latest_period() {
        let bars = vec![
            bar(0, 10, 11, 9, 100),
            bar(1, 12, 13, 11, 110),
            bar(2, 14, 15, 13, 120),
            bar(3, 16, 17, 15, 130),
        ];

        assert_eq!(
            simple_moving_average(&bars, 3).expect("sma should succeed"),
            Decimal::from(14)
        );
        assert_eq!(
            exponential_moving_average(&bars, 3).expect("ema should succeed"),
            Decimal::from(14)
        );
        assert_eq!(
            highest_high(&bars, 2).expect("highest high should succeed"),
            Decimal::from(17)
        );
        assert_eq!(
            lowest_low(&bars, 2).expect("lowest low should succeed"),
            Decimal::from(13)
        );
        assert_eq!(
            average_volume(&bars, 2).expect("avg volume should succeed"),
            Decimal::from(125)
        );
    }

    #[test]
    fn wick_helpers_measure_rejection_shapes() {
        let rejection_bar = BarInput {
            symbol: "GCM2026".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: Decimal::from(100),
            high: Decimal::from(104),
            low: Decimal::from(90),
            close: Decimal::from(103),
            volume: 150,
            closed_at: Utc::now(),
        };

        assert_eq!(bar_range(&rejection_bar), Decimal::from(14));
        assert_eq!(bar_body(&rejection_bar), Decimal::from(3));
        assert_eq!(upper_wick(&rejection_bar), Decimal::from(1));
        assert_eq!(lower_wick(&rejection_bar), Decimal::from(10));
        assert_eq!(
            close_position_ratio(&rejection_bar),
            Decimal::from(13) / Decimal::from(14)
        );
    }
}
