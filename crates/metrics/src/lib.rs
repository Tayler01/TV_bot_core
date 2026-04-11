//! Latency metric calculations and metric-store contracts.

use chrono::{DateTime, Utc};
use thiserror::Error;
use tv_bot_core_types::{TradePathLatencySnapshot, TradePathTimestamps};

pub const MODULE_STATUS: &str = "phase_6_foundation";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LatencyMetricError {
    #[error("timestamp `{later}` cannot be earlier than `{earlier}`")]
    TimestampOutOfOrder {
        earlier: &'static str,
        later: &'static str,
    },
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
    use chrono::{Duration, Utc};

    use super::*;

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
}
