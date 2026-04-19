use std::{collections::BTreeMap, num::NonZeroU64};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use databento::{
    dbn::{Record, RecordRefEnum, SType, Schema},
    historical::{timeseries::GetRangeParams, Client as HistoricalClient, HistoricalGateway},
};
use secrecy::{ExposeSecret, SecretString};
use tv_bot_core_types::{MarketEvent, Timeframe};

use crate::{event_timestamp, timeframe_duration, MarketDataError, MultiTimeframeAggregator};

const CHART_BACKFILL_LOOKBACK: ChronoDuration = ChronoDuration::days(5);
const CHART_BACKFILL_VISIBLE_WINDOW: ChronoDuration = ChronoDuration::hours(2);

pub async fn fetch_recent_chart_backfill(
    api_key: &SecretString,
    dataset: &str,
    raw_symbol: &str,
    requested_timeframes: &[Timeframe],
    now: DateTime<Utc>,
) -> Result<BTreeMap<Timeframe, Vec<MarketEvent>>, MarketDataError> {
    let mut client = HistoricalClient::new(
        api_key.expose_secret().to_owned(),
        HistoricalGateway::default(),
    )
    .map_err(|error| MarketDataError::TransportOperationFailed {
        operation: "historical_client",
        message: error.to_string(),
    })?;

    let one_minute_history =
        fetch_anchor_one_minute_history(&mut client, dataset, raw_symbol, now).await?;
    if one_minute_history.is_empty() {
        return Ok(BTreeMap::new());
    }

    let recent_one_minute = take_last_events(
        one_minute_history,
        chart_backfill_bar_target(Timeframe::OneMinute),
    );
    let latest_close = recent_one_minute
        .last()
        .map(event_timestamp)
        .ok_or_else(|| MarketDataError::TransportOperationFailed {
            operation: "historical_chart_backfill",
            message: "historical one-minute backfill produced no closing timestamp".to_owned(),
        })?;
    let backfill_start = latest_close - CHART_BACKFILL_VISIBLE_WINDOW + ChronoDuration::minutes(1);

    let mut bars = BTreeMap::new();
    let requested: Vec<Timeframe> = if requested_timeframes.is_empty() {
        vec![Timeframe::OneMinute]
    } else {
        requested_timeframes.to_vec()
    };

    if requested.contains(&Timeframe::OneMinute) || requested.contains(&Timeframe::FiveMinute) {
        bars.insert(Timeframe::OneMinute, recent_one_minute.clone());
    }

    if requested.contains(&Timeframe::OneSecond) {
        let one_second_history = fetch_historical_bars(
            &mut client,
            dataset,
            raw_symbol,
            Schema::Ohlcv1S,
            Timeframe::OneSecond,
            backfill_start,
            latest_close + ChronoDuration::seconds(1),
            Some(chart_backfill_bar_target(Timeframe::OneSecond)),
        )
        .await?;
        if !one_second_history.is_empty() {
            bars.insert(
                Timeframe::OneSecond,
                take_last_events(
                    one_second_history,
                    chart_backfill_bar_target(Timeframe::OneSecond),
                ),
            );
        }
    }

    if requested.contains(&Timeframe::FiveMinute) {
        let five_minute = aggregate_one_minute_bars(&recent_one_minute, Timeframe::FiveMinute)?;
        if !five_minute.is_empty() {
            bars.insert(
                Timeframe::FiveMinute,
                take_last_events(
                    five_minute,
                    chart_backfill_bar_target(Timeframe::FiveMinute),
                ),
            );
        }
    }

    Ok(bars)
}

async fn fetch_anchor_one_minute_history(
    client: &mut HistoricalClient,
    dataset: &str,
    raw_symbol: &str,
    now: DateTime<Utc>,
) -> Result<Vec<MarketEvent>, MarketDataError> {
    let primary_start = now - CHART_BACKFILL_LOOKBACK;
    match fetch_historical_bars(
        client,
        dataset,
        raw_symbol,
        Schema::Ohlcv1M,
        Timeframe::OneMinute,
        primary_start,
        now,
        None,
    )
    .await
    {
        Ok(bars) if !bars.is_empty() => Ok(bars),
        Ok(_) | Err(_) => {
            let historical_end = now - ChronoDuration::hours(24);
            if historical_end <= primary_start {
                return Ok(Vec::new());
            }

            fetch_historical_bars(
                client,
                dataset,
                raw_symbol,
                Schema::Ohlcv1M,
                Timeframe::OneMinute,
                primary_start,
                historical_end,
                None,
            )
            .await
        }
    }
}

async fn fetch_historical_bars(
    client: &mut HistoricalClient,
    dataset: &str,
    raw_symbol: &str,
    schema: Schema,
    timeframe: Timeframe,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    limit: Option<usize>,
) -> Result<Vec<MarketEvent>, MarketDataError> {
    if end <= start {
        return Ok(Vec::new());
    }

    let params = GetRangeParams::builder()
        .dataset(dataset)
        .symbols(raw_symbol.to_owned())
        .schema(schema)
        .date_time_range(start..end)
        .stype_in(SType::RawSymbol)
        .stype_out(SType::InstrumentId)
        .limit(limit.and_then(|value| NonZeroU64::new(value as u64)))
        .build();

    let mut decoder = client
        .timeseries()
        .get_range(&params)
        .await
        .map_err(|error| MarketDataError::TransportOperationFailed {
            operation: "historical_get_range",
            message: error.to_string(),
        })?;

    let mut bars = Vec::new();
    while let Some(record) = decoder.decode_record_ref().await.map_err(|error| {
        MarketDataError::TransportOperationFailed {
            operation: "historical_decode",
            message: error.to_string(),
        }
    })? {
        match record
            .as_enum()
            .map_err(|error| MarketDataError::TransportOperationFailed {
                operation: "historical_record_decode",
                message: error.to_string(),
            })? {
            RecordRefEnum::Ohlcv(message) => bars.push(MarketEvent::Bar {
                symbol: raw_symbol.to_owned(),
                timeframe,
                open: price_to_decimal(message.open),
                high: price_to_decimal(message.high),
                low: price_to_decimal(message.low),
                close: price_to_decimal(message.close),
                volume: message.volume,
                closed_at: record_timestamp(record)?,
            }),
            _ => {}
        }
    }

    Ok(bars)
}

fn aggregate_one_minute_bars(
    one_minute_bars: &[MarketEvent],
    target_timeframe: Timeframe,
) -> Result<Vec<MarketEvent>, MarketDataError> {
    let mut aggregator =
        MultiTimeframeAggregator::new(Timeframe::OneMinute, vec![target_timeframe])?;
    let mut aggregated = Vec::new();
    for event in one_minute_bars {
        aggregated.extend(aggregator.ingest(event));
    }
    aggregated.extend(aggregator.flush());
    aggregated.sort_by_key(event_timestamp);
    Ok(aggregated)
}

fn chart_backfill_bar_target(timeframe: Timeframe) -> usize {
    usize::try_from(
        (CHART_BACKFILL_VISIBLE_WINDOW.num_seconds() / timeframe_duration(timeframe).num_seconds())
            .max(1),
    )
    .unwrap_or(1)
}

fn take_last_events(events: Vec<MarketEvent>, count: usize) -> Vec<MarketEvent> {
    let total = events.len();
    if total <= count {
        return events;
    }
    events[total - count..].to_vec()
}

fn price_to_decimal(value: i64) -> rust_decimal::Decimal {
    rust_decimal::Decimal::from_i128_with_scale(i128::from(value), 9)
}

fn record_timestamp(
    record: databento::dbn::RecordRef<'_>,
) -> Result<DateTime<Utc>, MarketDataError> {
    let timestamp = record
        .index_ts()
        .ok_or(MarketDataError::MissingRecordTimestamp)?;

    DateTime::from_timestamp(timestamp.unix_timestamp(), timestamp.nanosecond())
        .ok_or(MarketDataError::MissingRecordTimestamp)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn minute_bar(closed_at: DateTime<Utc>, open: i64, close: i64) -> MarketEvent {
        MarketEvent::Bar {
            symbol: "SILK6".to_owned(),
            timeframe: Timeframe::OneMinute,
            open: price_to_decimal(open),
            high: price_to_decimal(open.max(close) + 5),
            low: price_to_decimal(open.min(close) - 5),
            close: price_to_decimal(close),
            volume: 10,
            closed_at,
        }
    }

    #[test]
    fn chart_backfill_target_covers_two_hours() {
        assert_eq!(chart_backfill_bar_target(Timeframe::OneSecond), 7_200);
        assert_eq!(chart_backfill_bar_target(Timeframe::OneMinute), 120);
        assert_eq!(chart_backfill_bar_target(Timeframe::FiveMinute), 24);
    }

    #[test]
    fn five_minute_backfill_aggregates_aligned_windows() {
        let base = Utc
            .with_ymd_and_hms(2026, 4, 17, 19, 0, 0)
            .single()
            .expect("timestamp should be valid");
        let one_minute = (0..10)
            .map(|index| {
                minute_bar(
                    base + ChronoDuration::minutes(index + 1),
                    1_560_00 + i64::from(index) * 10,
                    1_560_05 + i64::from(index) * 10,
                )
            })
            .collect::<Vec<_>>();

        let aggregated =
            aggregate_one_minute_bars(&one_minute, Timeframe::FiveMinute).expect("aggregation");

        assert_eq!(aggregated.len(), 2);
        assert_eq!(
            event_timestamp(&aggregated[0]),
            Utc.with_ymd_and_hms(2026, 4, 17, 19, 5, 0)
                .single()
                .expect("timestamp should be valid")
        );
        assert_eq!(
            event_timestamp(&aggregated[1]),
            Utc.with_ymd_and_hms(2026, 4, 17, 19, 10, 0)
                .single()
                .expect("timestamp should be valid")
        );
    }
}
