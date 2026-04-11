//! Latency metric calculations and runtime collectors.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use thiserror::Error;
use tv_bot_core_types::{TradePathLatencyRecord, TradePathLatencySnapshot, TradePathTimestamps};
use tv_bot_persistence::{PersistenceError, RuntimePersistence, TradeLatencyStore};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LatencyMetricError {
    #[error("timestamp `{later}` cannot be earlier than `{earlier}`")]
    TimestampOutOfOrder {
        earlier: &'static str,
        later: &'static str,
    },
}

#[derive(Debug, Error)]
pub enum RuntimeLatencyError {
    #[error("latency metric calculation failed: {source}")]
    Metric {
        #[source]
        source: LatencyMetricError,
    },
    #[error("latency persistence failed: {source}")]
    Persistence {
        #[source]
        source: PersistenceError,
    },
    #[error("latency collector lock is poisoned")]
    Poisoned,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeLatencySnapshot {
    pub latest_record: Option<TradePathLatencyRecord>,
    pub total_records: usize,
}

#[derive(Clone)]
pub struct RuntimeLatencyCollector {
    store: Arc<dyn TradeLatencyStore>,
    state: Arc<Mutex<RuntimeLatencyState>>,
}

#[derive(Clone, Debug, Default)]
struct RuntimeLatencyState {
    latest_record: Option<TradePathLatencyRecord>,
    total_records: usize,
}

impl RuntimeLatencyCollector {
    pub fn from_persistence(persistence: &RuntimePersistence) -> Result<Self, RuntimeLatencyError> {
        let store = persistence.trade_latency_store();
        let existing_records = store
            .list_trade_latencies()
            .map_err(|source| RuntimeLatencyError::Persistence { source })?;
        let latest_record = existing_records.last().cloned();
        let total_records = existing_records.len();

        Ok(Self {
            store,
            state: Arc::new(Mutex::new(RuntimeLatencyState {
                latest_record,
                total_records,
            })),
        })
    }

    pub fn record_trade_path(
        &self,
        action_id: String,
        strategy_id: Option<String>,
        timestamps: TradePathTimestamps,
        recorded_at: DateTime<Utc>,
    ) -> Result<TradePathLatencyRecord, RuntimeLatencyError> {
        let latency = calculate_trade_path_latency(&timestamps)
            .map_err(|source| RuntimeLatencyError::Metric { source })?;
        let record = TradePathLatencyRecord {
            action_id,
            strategy_id,
            recorded_at,
            timestamps,
            latency,
        };

        self.store
            .append_trade_latency(record.clone())
            .map_err(|source| RuntimeLatencyError::Persistence { source })?;

        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeLatencyError::Poisoned)?;
        state.latest_record = Some(record.clone());
        state.total_records += 1;

        Ok(record)
    }

    pub fn snapshot(&self) -> Result<RuntimeLatencySnapshot, RuntimeLatencyError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RuntimeLatencyError::Poisoned)?;

        Ok(RuntimeLatencySnapshot {
            latest_record: state.latest_record.clone(),
            total_records: state.total_records,
        })
    }
}

pub fn calculate_trade_path_latency(
    timestamps: &TradePathTimestamps,
) -> Result<TradePathLatencySnapshot, LatencyMetricError> {
    Ok(TradePathLatencySnapshot {
        signal_latency_ms: latency_between(
            timestamps.market_event_at,
            timestamps.signal_at,
            "market_event_at",
            "signal_at",
        )?,
        decision_latency_ms: latency_between(
            timestamps.signal_at,
            timestamps.decision_at,
            "signal_at",
            "decision_at",
        )?,
        order_send_latency_ms: latency_between(
            timestamps.decision_at,
            timestamps.order_sent_at,
            "decision_at",
            "order_sent_at",
        )?,
        broker_ack_latency_ms: latency_between(
            timestamps.order_sent_at,
            timestamps.broker_ack_at,
            "order_sent_at",
            "broker_ack_at",
        )?,
        fill_latency_ms: latency_between(
            timestamps.broker_ack_at,
            timestamps.fill_at,
            "broker_ack_at",
            "fill_at",
        )?,
        sync_update_latency_ms: latency_between(
            timestamps.fill_at,
            timestamps.sync_update_at,
            "fill_at",
            "sync_update_at",
        )?,
        end_to_end_fill_latency_ms: latency_between(
            timestamps.market_event_at,
            timestamps.fill_at,
            "market_event_at",
            "fill_at",
        )?,
        end_to_end_sync_latency_ms: latency_between(
            timestamps.market_event_at,
            timestamps.sync_update_at,
            "market_event_at",
            "sync_update_at",
        )?,
    })
}

fn latency_between(
    earlier: Option<DateTime<Utc>>,
    later: Option<DateTime<Utc>>,
    earlier_label: &'static str,
    later_label: &'static str,
) -> Result<Option<u64>, LatencyMetricError> {
    let (Some(earlier), Some(later)) = (earlier, later) else {
        return Ok(None);
    };

    let delta = later.signed_duration_since(earlier);
    if delta.num_milliseconds() < 0 {
        return Err(LatencyMetricError::TimestampOutOfOrder {
            earlier: earlier_label,
            later: later_label,
        });
    }

    Ok(Some(delta.num_milliseconds() as u64))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{Duration, Utc};
    use tv_bot_config::{AppConfig, ConfigError, MapEnvironment};

    use super::*;

    fn load_test_config(values: &[(&str, &str)]) -> Result<AppConfig, ConfigError> {
        let env = values
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>();

        AppConfig::load(None, &MapEnvironment::new(env))
    }

    #[test]
    fn calculates_trade_path_latency_for_complete_chain() {
        let base = Utc::now();
        let timestamps = TradePathTimestamps {
            market_event_at: Some(base),
            signal_at: Some(base + Duration::milliseconds(3)),
            decision_at: Some(base + Duration::milliseconds(7)),
            order_sent_at: Some(base + Duration::milliseconds(9)),
            broker_ack_at: Some(base + Duration::milliseconds(15)),
            fill_at: Some(base + Duration::milliseconds(27)),
            sync_update_at: Some(base + Duration::milliseconds(41)),
        };

        let latency = calculate_trade_path_latency(&timestamps).expect("latency should compute");
        assert_eq!(latency.signal_latency_ms, Some(3));
        assert_eq!(latency.decision_latency_ms, Some(4));
        assert_eq!(latency.order_send_latency_ms, Some(2));
        assert_eq!(latency.broker_ack_latency_ms, Some(6));
        assert_eq!(latency.fill_latency_ms, Some(12));
        assert_eq!(latency.sync_update_latency_ms, Some(14));
        assert_eq!(latency.end_to_end_fill_latency_ms, Some(27));
        assert_eq!(latency.end_to_end_sync_latency_ms, Some(41));
    }

    #[test]
    fn leaves_missing_segments_empty_without_failing() {
        let base = Utc::now();
        let timestamps = TradePathTimestamps {
            market_event_at: Some(base),
            signal_at: None,
            decision_at: None,
            order_sent_at: Some(base + Duration::milliseconds(12)),
            broker_ack_at: Some(base + Duration::milliseconds(16)),
            fill_at: Some(base + Duration::milliseconds(33)),
            sync_update_at: None,
        };

        let latency = calculate_trade_path_latency(&timestamps).expect("latency should compute");
        assert_eq!(latency.signal_latency_ms, None);
        assert_eq!(latency.decision_latency_ms, None);
        assert_eq!(latency.order_send_latency_ms, None);
        assert_eq!(latency.broker_ack_latency_ms, Some(4));
        assert_eq!(latency.fill_latency_ms, Some(17));
        assert_eq!(latency.end_to_end_fill_latency_ms, Some(33));
        assert_eq!(latency.end_to_end_sync_latency_ms, None);
    }

    #[test]
    fn rejects_out_of_order_timestamps() {
        let base = Utc::now();
        let timestamps = TradePathTimestamps {
            market_event_at: Some(base),
            signal_at: Some(base + Duration::milliseconds(5)),
            decision_at: Some(base + Duration::milliseconds(4)),
            order_sent_at: None,
            broker_ack_at: None,
            fill_at: None,
            sync_update_at: None,
        };

        let error =
            calculate_trade_path_latency(&timestamps).expect_err("out-of-order timestamps fail");
        assert_eq!(
            error,
            LatencyMetricError::TimestampOutOfOrder {
                earlier: "signal_at",
                later: "decision_at",
            }
        );
    }

    #[test]
    fn equal_timestamps_produce_zero_latency() {
        let base = Utc::now();
        let timestamps = TradePathTimestamps {
            market_event_at: Some(base),
            signal_at: Some(base),
            decision_at: Some(base),
            order_sent_at: Some(base),
            broker_ack_at: Some(base),
            fill_at: Some(base),
            sync_update_at: Some(base),
        };

        let latency = calculate_trade_path_latency(&timestamps).expect("latency should compute");
        assert_eq!(latency.signal_latency_ms, Some(0));
        assert_eq!(latency.decision_latency_ms, Some(0));
        assert_eq!(latency.order_send_latency_ms, Some(0));
        assert_eq!(latency.broker_ack_latency_ms, Some(0));
        assert_eq!(latency.fill_latency_ms, Some(0));
        assert_eq!(latency.sync_update_latency_ms, Some(0));
        assert_eq!(latency.end_to_end_fill_latency_ms, Some(0));
        assert_eq!(latency.end_to_end_sync_latency_ms, Some(0));
    }

    #[test]
    fn runtime_latency_collector_persists_and_tracks_latest_record() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");
        let persistence = RuntimePersistence::open(&config);
        let collector = RuntimeLatencyCollector::from_persistence(&persistence)
            .expect("collector should initialize");
        let recorded_at = Utc::now();

        let record = collector
            .record_trade_path(
                "act-1".to_owned(),
                Some("gc_phase_6_v1".to_owned()),
                TradePathTimestamps {
                    market_event_at: Some(recorded_at),
                    signal_at: Some(recorded_at + Duration::milliseconds(2)),
                    decision_at: Some(recorded_at + Duration::milliseconds(5)),
                    order_sent_at: Some(recorded_at + Duration::milliseconds(7)),
                    broker_ack_at: Some(recorded_at + Duration::milliseconds(11)),
                    fill_at: None,
                    sync_update_at: None,
                },
                recorded_at,
            )
            .expect("latency should persist");

        let snapshot = collector
            .snapshot()
            .expect("collector snapshot should load");
        assert_eq!(snapshot.total_records, 1);
        assert_eq!(snapshot.latest_record, Some(record.clone()));
        assert_eq!(
            persistence
                .trade_latency_store()
                .list_trade_latencies()
                .expect("latencies should list"),
            vec![record]
        );
    }

    #[test]
    fn runtime_latency_collector_hydrates_existing_records() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");
        let persistence = RuntimePersistence::open(&config);
        let recorded_at = Utc::now();
        let existing = TradePathLatencyRecord {
            action_id: "act-existing".to_owned(),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            recorded_at,
            timestamps: TradePathTimestamps {
                market_event_at: Some(recorded_at),
                signal_at: Some(recorded_at + Duration::milliseconds(1)),
                decision_at: Some(recorded_at + Duration::milliseconds(2)),
                order_sent_at: Some(recorded_at + Duration::milliseconds(3)),
                broker_ack_at: Some(recorded_at + Duration::milliseconds(4)),
                fill_at: None,
                sync_update_at: None,
            },
            latency: TradePathLatencySnapshot {
                signal_latency_ms: Some(1),
                decision_latency_ms: Some(1),
                order_send_latency_ms: Some(1),
                broker_ack_latency_ms: Some(1),
                fill_latency_ms: None,
                sync_update_latency_ms: None,
                end_to_end_fill_latency_ms: None,
                end_to_end_sync_latency_ms: None,
            },
        };
        persistence
            .trade_latency_store()
            .append_trade_latency(existing.clone())
            .expect("existing latency should append");

        let collector = RuntimeLatencyCollector::from_persistence(&persistence)
            .expect("collector should initialize");
        let snapshot = collector
            .snapshot()
            .expect("collector snapshot should load");
        assert_eq!(snapshot.total_records, 1);
        assert_eq!(snapshot.latest_record, Some(existing));
    }
}
