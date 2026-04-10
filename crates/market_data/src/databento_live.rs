use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use databento::{
    dbn::{
        PitSymbolMap, RType, Record, RecordRef, RecordRefEnum, SType, Schema, SymbolIndex,
        SystemCode,
    },
    live::Subscription,
    LiveClient,
};
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, info, warn};
use tv_bot_core_types::{DatabentoSymbology, FeedType, MarketEvent, Timeframe};

use crate::{DatabentoTransport, DatabentoTransportUpdate, MarketDataError, SubscriptionRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabentoSlowReaderPolicy {
    MarkDegraded,
}

#[derive(Clone, Debug)]
pub struct DatabentoLiveTransportConfig {
    pub api_key: SecretString,
    pub gateway_address: Option<String>,
    pub slow_reader_policy: DatabentoSlowReaderPolicy,
}

impl DatabentoLiveTransportConfig {
    pub fn new(api_key: SecretString) -> Self {
        Self {
            api_key,
            gateway_address: None,
            slow_reader_policy: DatabentoSlowReaderPolicy::MarkDegraded,
        }
    }

    pub fn with_gateway_address(mut self, gateway_address: impl Into<String>) -> Self {
        self.gateway_address = Some(gateway_address.into());
        self
    }
}

pub struct DatabentoLiveTransport {
    config: DatabentoLiveTransportConfig,
    client: Option<LiveClient>,
    symbol_map: PitSymbolMap,
    dataset: Option<String>,
    started: bool,
}

impl DatabentoLiveTransport {
    pub fn new(config: DatabentoLiveTransportConfig) -> Self {
        Self {
            config,
            client: None,
            symbol_map: PitSymbolMap::default(),
            dataset: None,
            started: false,
        }
    }

    fn client_mut(&mut self) -> Result<&mut LiveClient, MarketDataError> {
        self.client
            .as_mut()
            .ok_or_else(|| MarketDataError::TransportOperationFailed {
                operation: "client",
                message: "Databento live client is not connected".to_owned(),
            })
    }
}

#[async_trait]
impl DatabentoTransport for DatabentoLiveTransport {
    async fn connect(&mut self, dataset: &str) -> Result<(), MarketDataError> {
        let mut builder = LiveClient::builder()
            .key(self.config.api_key.expose_secret().to_owned())
            .map_err(|error| MarketDataError::TransportOperationFailed {
                operation: "connect",
                message: error.to_string(),
            })?
            .dataset(dataset);

        if let Some(address) = &self.config.gateway_address {
            builder = builder.addr(address).await.map_err(|error| {
                MarketDataError::InvalidGatewayAddress {
                    address: address.clone(),
                    message: error.to_string(),
                }
            })?;
        }

        let client =
            builder
                .build()
                .await
                .map_err(|error| MarketDataError::TransportOperationFailed {
                    operation: "connect",
                    message: error.to_string(),
                })?;

        self.client = Some(client);
        self.symbol_map = PitSymbolMap::default();
        self.dataset = Some(dataset.to_owned());
        self.started = false;

        info!(dataset, "Databento live transport connected");
        Ok(())
    }

    async fn subscribe(&mut self, request: &SubscriptionRequest) -> Result<(), MarketDataError> {
        let subscriptions = build_live_subscriptions(request)?;
        let client = self.client_mut()?;

        for subscription in subscriptions {
            debug!(
                dataset = %request.dataset,
                "subscribing Databento live feed"
            );
            client.subscribe(subscription).await.map_err(|error| {
                MarketDataError::TransportOperationFailed {
                    operation: "subscribe",
                    message: error.to_string(),
                }
            })?;
        }

        Ok(())
    }

    async fn start(&mut self) -> Result<(), MarketDataError> {
        let client = self.client_mut()?;
        client
            .start()
            .await
            .map_err(|error| MarketDataError::TransportOperationFailed {
                operation: "start",
                message: error.to_string(),
            })?;
        self.started = true;

        info!("Databento live transport started");
        Ok(())
    }

    async fn next_update(&mut self) -> Result<Option<DatabentoTransportUpdate>, MarketDataError> {
        let dataset = self.dataset.clone().unwrap_or_default();
        let slow_reader_policy = self.config.slow_reader_policy;
        let client =
            self.client
                .as_mut()
                .ok_or_else(|| MarketDataError::TransportOperationFailed {
                    operation: "client",
                    message: "Databento live client is not connected".to_owned(),
                })?;
        let record = {
            client.next_record().await.map_err(|error| {
                MarketDataError::TransportOperationFailed {
                    operation: "next_record",
                    message: error.to_string(),
                }
            })?
        };

        let Some(record) = record else {
            return Ok(None);
        };

        decode_record(record, &mut self.symbol_map, &dataset, slow_reader_policy)
    }

    async fn disconnect(&mut self) -> Result<(), MarketDataError> {
        if let Some(client) = self.client.as_mut() {
            client
                .close()
                .await
                .map_err(|error| MarketDataError::TransportOperationFailed {
                    operation: "disconnect",
                    message: error.to_string(),
                })?;
        }

        self.client = None;
        self.symbol_map = PitSymbolMap::default();
        self.dataset = None;
        self.started = false;

        info!("Databento live transport disconnected");
        Ok(())
    }
}

fn build_live_subscriptions(
    request: &SubscriptionRequest,
) -> Result<Vec<Subscription>, MarketDataError> {
    let mut grouped_symbols: BTreeMap<DatabentoSymbology, Vec<String>> = BTreeMap::new();
    for instrument in &request.instruments {
        grouped_symbols
            .entry(instrument.symbology)
            .or_default()
            .push(instrument.symbol.clone());
    }

    let mut subscriptions = Vec::new();
    for (symbology, symbols) in grouped_symbols {
        let stype_in = stype_for_symbology(symbology);

        for feed in &request.feeds {
            let builder = Subscription::builder()
                .schema(schema_for_feed(*feed)?)
                .stype_in(stype_in)
                .symbols(symbols.clone());

            subscriptions.push(if let Some(replay_from) = request.replay_from {
                builder.start(replay_from).build()
            } else {
                builder.build()
            });
        }
    }

    Ok(subscriptions)
}

fn schema_for_feed(feed: FeedType) -> Result<Schema, MarketDataError> {
    match feed {
        FeedType::Trades => Ok(Schema::Trades),
        FeedType::Ohlcv1s => Ok(Schema::Ohlcv1S),
        FeedType::Ohlcv1m => Ok(Schema::Ohlcv1M),
        FeedType::Mbp => Ok(Schema::Mbp1),
        FeedType::Mbo | FeedType::Ohlcv5m => Err(MarketDataError::UnsupportedLiveFeed { feed }),
    }
}

fn stype_for_symbology(symbology: DatabentoSymbology) -> SType {
    match symbology {
        DatabentoSymbology::RawSymbol => SType::RawSymbol,
        DatabentoSymbology::Parent => SType::Parent,
        DatabentoSymbology::Continuous => SType::Continuous,
    }
}

fn decode_record(
    record: RecordRef<'_>,
    symbol_map: &mut PitSymbolMap,
    dataset: &str,
    slow_reader_policy: DatabentoSlowReaderPolicy,
) -> Result<Option<DatabentoTransportUpdate>, MarketDataError> {
    match record
        .as_enum()
        .map_err(|error| MarketDataError::TransportOperationFailed {
            operation: "record_decode",
            message: error.to_string(),
        })? {
        RecordRefEnum::SymbolMapping(_) | RecordRefEnum::InstrumentDef(_) => {
            symbol_map.on_record(record).map_err(|error| {
                MarketDataError::TransportOperationFailed {
                    operation: "symbol_map_update",
                    message: error.to_string(),
                }
            })?;
            Ok(None)
        }
        RecordRefEnum::Trade(message) => {
            Ok(Some(DatabentoTransportUpdate::Event(MarketEvent::Trade {
                symbol: resolve_symbol(symbol_map, message)?,
                price: price_to_decimal(message.price),
                quantity: u64::from(message.size),
                occurred_at: record_timestamp(record)?,
            })))
        }
        RecordRefEnum::Ohlcv(message) => {
            Ok(Some(DatabentoTransportUpdate::Event(MarketEvent::Bar {
                symbol: resolve_symbol(symbol_map, message)?,
                timeframe: timeframe_for_record(record.rtype().map_err(|error| {
                    MarketDataError::TransportOperationFailed {
                        operation: "record_rtype",
                        message: error.to_string(),
                    }
                })?)?,
                open: price_to_decimal(message.open),
                high: price_to_decimal(message.high),
                low: price_to_decimal(message.low),
                close: price_to_decimal(message.close),
                volume: message.volume,
                closed_at: record_timestamp(record)?,
            })))
        }
        RecordRefEnum::System(message) => Ok(Some(decode_system_message(
            dataset,
            record_timestamp(record)?,
            message
                .code()
                .map_err(|error| MarketDataError::TransportOperationFailed {
                    operation: "system_code",
                    message: error.to_string(),
                })?,
            message
                .msg()
                .map_err(|error| MarketDataError::TransportOperationFailed {
                    operation: "system_message",
                    message: error.to_string(),
                })?,
            slow_reader_policy,
        ))),
        RecordRefEnum::Error(message) => Err(MarketDataError::TransportOperationFailed {
            operation: "gateway_error",
            message: message
                .err()
                .map_err(|error| MarketDataError::TransportOperationFailed {
                    operation: "gateway_error",
                    message: error.to_string(),
                })?
                .to_owned(),
        }),
        other => Err(MarketDataError::TransportOperationFailed {
            operation: "record_decode",
            message: format!("unsupported live record type: {other:?}"),
        }),
    }
}

fn decode_system_message(
    dataset: &str,
    occurred_at: DateTime<Utc>,
    code: SystemCode,
    detail: &str,
    slow_reader_policy: DatabentoSlowReaderPolicy,
) -> DatabentoTransportUpdate {
    match code {
        SystemCode::Heartbeat => DatabentoTransportUpdate::Event(MarketEvent::Heartbeat {
            dataset: dataset.to_owned(),
            occurred_at,
        }),
        SystemCode::ReplayCompleted => DatabentoTransportUpdate::ReplayCompleted {
            occurred_at,
            detail: detail.to_owned(),
        },
        SystemCode::EndOfInterval => DatabentoTransportUpdate::EndOfInterval {
            occurred_at,
            detail: detail.to_owned(),
        },
        SystemCode::SlowReaderWarning => {
            if matches!(slow_reader_policy, DatabentoSlowReaderPolicy::MarkDegraded) {
                warn!(%detail, "Databento slow-reader warning");
            }
            DatabentoTransportUpdate::SlowReaderWarning {
                occurred_at,
                detail: detail.to_owned(),
            }
        }
        _ => DatabentoTransportUpdate::SubscriptionAck {
            occurred_at,
            detail: detail.to_owned(),
        },
    }
}

fn resolve_symbol<R: Record>(
    symbol_map: &PitSymbolMap,
    record: &R,
) -> Result<String, MarketDataError> {
    symbol_map.get_for_rec(record).cloned().ok_or_else(|| {
        MarketDataError::MissingInstrumentSymbol {
            instrument_id: record.header().instrument_id,
        }
    })
}

fn timeframe_for_record(rtype: RType) -> Result<Timeframe, MarketDataError> {
    match rtype {
        RType::Ohlcv1S => Ok(Timeframe::OneSecond),
        RType::Ohlcv1M => Ok(Timeframe::OneMinute),
        other => Err(MarketDataError::TransportOperationFailed {
            operation: "record_rtype",
            message: format!("unsupported OHLCV record type `{other:?}`"),
        }),
    }
}

fn price_to_decimal(value: i64) -> Decimal {
    Decimal::from_i128_with_scale(i128::from(value), 9)
}

fn record_timestamp(record: RecordRef<'_>) -> Result<DateTime<Utc>, MarketDataError> {
    let timestamp = record
        .index_ts()
        .ok_or(MarketDataError::MissingRecordTimestamp)?;

    DateTime::from_timestamp(timestamp.unix_timestamp(), timestamp.nanosecond())
        .ok_or(MarketDataError::MissingRecordTimestamp)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use tv_bot_core_types::{DatabentoInstrument, DatabentoSymbology};

    use super::*;

    fn request_with(feed: FeedType, symbology: DatabentoSymbology) -> SubscriptionRequest {
        SubscriptionRequest {
            provider: "databento",
            dataset: "GLBX.MDP3".to_owned(),
            instruments: vec![DatabentoInstrument {
                dataset: "GLBX.MDP3".to_owned(),
                symbol: "GCM2026".to_owned(),
                symbology,
            }],
            feeds: vec![feed],
            timeframes: vec![Timeframe::OneMinute],
            replay_from: None,
        }
    }

    #[test]
    fn builds_live_subscriptions_for_supported_feed_and_symbology() {
        let request = request_with(FeedType::Ohlcv1m, DatabentoSymbology::RawSymbol);

        let subscriptions = build_live_subscriptions(&request).expect("subscriptions should build");

        assert_eq!(subscriptions.len(), 1);
    }

    #[test]
    fn rejects_unsupported_live_feed() {
        let error = build_live_subscriptions(&request_with(
            FeedType::Ohlcv5m,
            DatabentoSymbology::RawSymbol,
        ))
        .expect_err("5m feed should be aggregated locally, not subscribed directly");

        assert_eq!(
            error,
            MarketDataError::UnsupportedLiveFeed {
                feed: FeedType::Ohlcv5m,
            }
        );
    }

    #[test]
    fn heartbeat_system_message_becomes_runtime_heartbeat_event() {
        let update = decode_system_message(
            "GLBX.MDP3",
            Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap(),
            SystemCode::Heartbeat,
            "heartbeat",
            DatabentoSlowReaderPolicy::MarkDegraded,
        );

        assert_eq!(
            update,
            DatabentoTransportUpdate::Event(MarketEvent::Heartbeat {
                dataset: "GLBX.MDP3".to_owned(),
                occurred_at: Utc.with_ymd_and_hms(2026, 4, 10, 13, 0, 0).unwrap(),
            })
        );
    }
}
