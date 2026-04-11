//! Event-sourced runtime state and trading-history projections.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tv_bot_core_types::{
    ActionSource, EventJournalRecord, FillRecord, OrderRecord, PnlSnapshotRecord, PositionRecord,
    RiskDecisionStatus, RuntimeMode, StrategyRunRecord, StrategyRunStatus, TradeSummaryRecord,
    TradeSummaryStatus,
};

pub const MODULE_STATUS: &str = "phase_6_projection_and_history";

pub trait EventProjectionStore: Send + Sync {
    fn apply_event(&self, record: EventJournalRecord) -> Result<(), StateStoreError>;
    fn snapshot(&self) -> Result<ProjectedRuntimeState, StateStoreError>;
    fn rebuild_from_events(&self, records: &[EventJournalRecord]) -> Result<(), StateStoreError>;
}

pub trait TradingHistoryProjectionStore: Send + Sync {
    fn apply_strategy_run(&self, record: StrategyRunRecord) -> Result<(), StateStoreError>;
    fn apply_order(&self, record: OrderRecord) -> Result<(), StateStoreError>;
    fn apply_fill(&self, record: FillRecord) -> Result<(), StateStoreError>;
    fn apply_position(&self, record: PositionRecord) -> Result<(), StateStoreError>;
    fn apply_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), StateStoreError>;
    fn apply_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), StateStoreError>;
    fn snapshot_history(&self) -> Result<ProjectedTradingHistoryState, StateStoreError>;
    fn rebuild_from_records(&self, records: &TradingHistoryRecords) -> Result<(), StateStoreError>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedRuntimeState {
    pub total_events: u64,
    pub last_event_id: Option<String>,
    pub last_occurred_at: Option<DateTime<Utc>>,
    pub last_category: Option<String>,
    pub last_action: Option<String>,
    pub last_source: Option<ActionSource>,
    pub last_mode: Option<RuntimeMode>,
    pub last_strategy_id: Option<String>,
    pub last_intent: Option<String>,
    pub last_risk_decision_status: Option<RiskDecisionStatus>,
    pub hard_override_required_count: u64,
    pub hard_override_used_count: u64,
    pub dispatch_succeeded_count: u64,
    pub dispatch_skipped_count: u64,
    pub dispatch_failed_count: u64,
    pub manual_event_count: u64,
    pub strategy_event_count: u64,
    pub last_error: Option<String>,
}

impl ProjectedRuntimeState {
    pub fn apply(&mut self, record: &EventJournalRecord) {
        self.total_events += 1;
        self.last_event_id = Some(record.event_id.clone());
        self.last_occurred_at = Some(record.occurred_at);
        self.last_category = Some(record.category.clone());
        self.last_action = Some(record.action.clone());
        self.last_source = Some(record.source);

        match record.source {
            ActionSource::System => self.strategy_event_count += 1,
            ActionSource::Dashboard | ActionSource::Cli => self.manual_event_count += 1,
        }

        if let Some(mode) = payload_enum::<RuntimeMode>(&record.payload, "mode") {
            self.last_mode = Some(mode);
        }
        if let Some(strategy_id) = payload_string(&record.payload, "strategy_id") {
            self.last_strategy_id = Some(strategy_id);
        }
        if let Some(intent) = payload_string(&record.payload, "intent") {
            self.last_intent = Some(intent);
        }
        if let Some(decision_status) =
            payload_enum::<RiskDecisionStatus>(&record.payload, "decision_status")
        {
            self.last_risk_decision_status = Some(decision_status);
        }
        if record.action == "hard_override_required" {
            self.hard_override_required_count += 1;
        }
        if record.action == "hard_override_used" {
            self.hard_override_used_count += 1;
        }
        if record.action == "dispatch_succeeded" {
            self.dispatch_succeeded_count += 1;
        }
        if record.action == "dispatch_skipped" {
            self.dispatch_skipped_count += 1;
        }
        if record.action == "dispatch_failed" {
            self.dispatch_failed_count += 1;
        }
        if let Some(error) = payload_string(&record.payload, "error")
            .or_else(|| payload_string(&record.payload, "reason"))
        {
            self.last_error = Some(error);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradingHistoryRecords {
    pub strategy_runs: Vec<StrategyRunRecord>,
    pub orders: Vec<OrderRecord>,
    pub fills: Vec<FillRecord>,
    pub positions: Vec<PositionRecord>,
    pub pnl_snapshots: Vec<PnlSnapshotRecord>,
    pub trade_summaries: Vec<TradeSummaryRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedTradingHistoryState {
    pub total_strategy_run_records: u64,
    pub total_order_records: u64,
    pub total_fill_records: u64,
    pub total_position_records: u64,
    pub total_pnl_snapshot_records: u64,
    pub total_trade_summary_records: u64,
    pub strategy_runs: BTreeMap<String, StrategyRunRecord>,
    pub active_run_ids: Vec<String>,
    pub orders: BTreeMap<String, OrderRecord>,
    pub working_order_ids: Vec<String>,
    pub fills: BTreeMap<String, FillRecord>,
    pub positions: BTreeMap<String, PositionRecord>,
    pub open_position_symbols: Vec<String>,
    pub latest_run: Option<StrategyRunRecord>,
    pub latest_order: Option<OrderRecord>,
    pub latest_fill: Option<FillRecord>,
    pub latest_position: Option<PositionRecord>,
    pub latest_pnl_snapshot: Option<PnlSnapshotRecord>,
    pub trade_summaries: BTreeMap<String, TradeSummaryRecord>,
    pub open_trade_ids: Vec<String>,
    pub latest_trade_summary: Option<TradeSummaryRecord>,
    pub closed_trade_count: u64,
    pub cancelled_trade_count: u64,
    pub closed_trade_gross_pnl: Decimal,
    pub closed_trade_net_pnl: Decimal,
    pub closed_trade_fees: Decimal,
    pub closed_trade_commissions: Decimal,
    pub closed_trade_slippage: Decimal,
    pub recorded_fill_fees: Decimal,
    pub recorded_fill_commissions: Decimal,
    pub last_activity_at: Option<DateTime<Utc>>,
}

impl ProjectedTradingHistoryState {
    pub fn apply_strategy_run(&mut self, record: &StrategyRunRecord) {
        self.total_strategy_run_records += 1;
        self.strategy_runs
            .insert(record.run_id.clone(), record.clone());
        self.active_run_ids = self
            .strategy_runs
            .iter()
            .filter(|(_, run)| {
                matches!(
                    run.status,
                    StrategyRunStatus::Starting
                        | StrategyRunStatus::Active
                        | StrategyRunStatus::Paused
                )
            })
            .map(|(run_id, _)| run_id.clone())
            .collect();
        self.replace_latest_run(record);
        self.update_last_activity(record.ended_at.unwrap_or(record.started_at));
    }

    pub fn apply_order(&mut self, record: &OrderRecord) {
        self.total_order_records += 1;
        self.orders
            .insert(record.broker_order_id.clone(), record.clone());
        self.working_order_ids = self
            .orders
            .iter()
            .filter(|(_, order)| {
                matches!(
                    order.status,
                    tv_bot_core_types::BrokerOrderStatus::Pending
                        | tv_bot_core_types::BrokerOrderStatus::Working
                )
            })
            .map(|(order_id, _)| order_id.clone())
            .collect();
        self.replace_latest_order(record);
        self.update_last_activity(record.updated_at);
    }

    pub fn apply_fill(&mut self, record: &FillRecord) {
        self.total_fill_records += 1;
        self.fills.insert(record.fill_id.clone(), record.clone());
        self.recompute_fill_totals();
        self.replace_latest_fill(record);
        self.update_last_activity(record.occurred_at);
    }

    pub fn apply_position(&mut self, record: &PositionRecord) {
        self.total_position_records += 1;
        if record.quantity == 0 {
            self.positions.remove(&record.symbol);
        } else {
            self.positions.insert(record.symbol.clone(), record.clone());
        }
        self.open_position_symbols = self.positions.keys().cloned().collect();
        self.replace_latest_position(record);
        self.update_last_activity(record.captured_at);
    }

    pub fn apply_pnl_snapshot(&mut self, record: &PnlSnapshotRecord) {
        self.total_pnl_snapshot_records += 1;
        self.replace_latest_pnl_snapshot(record);
        self.update_last_activity(record.captured_at);
    }

    pub fn apply_trade_summary(&mut self, record: &TradeSummaryRecord) {
        self.total_trade_summary_records += 1;
        self.trade_summaries
            .insert(record.trade_id.clone(), record.clone());
        self.recompute_trade_totals();
        self.replace_latest_trade_summary(record);
        self.update_last_activity(record.closed_at.unwrap_or(record.opened_at));
    }

    fn recompute_fill_totals(&mut self) {
        self.recorded_fill_fees = Decimal::ZERO;
        self.recorded_fill_commissions = Decimal::ZERO;

        for fill in self.fills.values() {
            self.recorded_fill_fees += fill.fee;
            self.recorded_fill_commissions += fill.commission;
        }
    }

    fn recompute_trade_totals(&mut self) {
        self.open_trade_ids.clear();
        self.closed_trade_count = 0;
        self.cancelled_trade_count = 0;
        self.closed_trade_gross_pnl = Decimal::ZERO;
        self.closed_trade_net_pnl = Decimal::ZERO;
        self.closed_trade_fees = Decimal::ZERO;
        self.closed_trade_commissions = Decimal::ZERO;
        self.closed_trade_slippage = Decimal::ZERO;

        for (trade_id, summary) in &self.trade_summaries {
            match summary.status {
                TradeSummaryStatus::Open => self.open_trade_ids.push(trade_id.clone()),
                TradeSummaryStatus::Closed => {
                    self.closed_trade_count += 1;
                    self.closed_trade_gross_pnl += summary.gross_pnl;
                    self.closed_trade_net_pnl += summary.net_pnl;
                    self.closed_trade_fees += summary.fees;
                    self.closed_trade_commissions += summary.commissions;
                    self.closed_trade_slippage += summary.slippage;
                }
                TradeSummaryStatus::Cancelled => {
                    self.cancelled_trade_count += 1;
                }
            }
        }
    }

    fn update_last_activity(&mut self, candidate: DateTime<Utc>) {
        if self
            .last_activity_at
            .map(|current| candidate >= current)
            .unwrap_or(true)
        {
            self.last_activity_at = Some(candidate);
        }
    }

    fn replace_latest_run(&mut self, candidate: &StrategyRunRecord) {
        let candidate_at = candidate.ended_at.unwrap_or(candidate.started_at);
        if self
            .latest_run
            .as_ref()
            .map(|current| candidate_at >= current.ended_at.unwrap_or(current.started_at))
            .unwrap_or(true)
        {
            self.latest_run = Some(candidate.clone());
        }
    }

    fn replace_latest_order(&mut self, candidate: &OrderRecord) {
        if self
            .latest_order
            .as_ref()
            .map(|current| candidate.updated_at >= current.updated_at)
            .unwrap_or(true)
        {
            self.latest_order = Some(candidate.clone());
        }
    }

    fn replace_latest_fill(&mut self, candidate: &FillRecord) {
        if self
            .latest_fill
            .as_ref()
            .map(|current| candidate.occurred_at >= current.occurred_at)
            .unwrap_or(true)
        {
            self.latest_fill = Some(candidate.clone());
        }
    }

    fn replace_latest_position(&mut self, candidate: &PositionRecord) {
        if self
            .latest_position
            .as_ref()
            .map(|current| candidate.captured_at >= current.captured_at)
            .unwrap_or(true)
        {
            self.latest_position = Some(candidate.clone());
        }
    }

    fn replace_latest_pnl_snapshot(&mut self, candidate: &PnlSnapshotRecord) {
        if self
            .latest_pnl_snapshot
            .as_ref()
            .map(|current| candidate.captured_at >= current.captured_at)
            .unwrap_or(true)
        {
            self.latest_pnl_snapshot = Some(candidate.clone());
        }
    }

    fn replace_latest_trade_summary(&mut self, candidate: &TradeSummaryRecord) {
        let candidate_at = candidate.closed_at.unwrap_or(candidate.opened_at);
        if self
            .latest_trade_summary
            .as_ref()
            .map(|current| candidate_at >= current.closed_at.unwrap_or(current.opened_at))
            .unwrap_or(true)
        {
            self.latest_trade_summary = Some(candidate.clone());
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryStateStore {
    inner: Arc<Mutex<ProjectedRuntimeState>>,
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventProjectionStore for InMemoryStateStore {
    fn apply_event(&self, record: EventJournalRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply(&record);
        Ok(())
    }

    fn snapshot(&self) -> Result<ProjectedRuntimeState, StateStoreError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| StateStoreError::Poisoned)?
            .clone())
    }

    fn rebuild_from_events(&self, records: &[EventJournalRecord]) -> Result<(), StateStoreError> {
        let mut projection = ProjectedRuntimeState::default();
        for record in records {
            projection.apply(record);
        }

        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        *guard = projection;
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryTradingHistoryStore {
    inner: Arc<Mutex<ProjectedTradingHistoryState>>,
}

impl InMemoryTradingHistoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TradingHistoryProjectionStore for InMemoryTradingHistoryStore {
    fn apply_strategy_run(&self, record: StrategyRunRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply_strategy_run(&record);
        Ok(())
    }

    fn apply_order(&self, record: OrderRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply_order(&record);
        Ok(())
    }

    fn apply_fill(&self, record: FillRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply_fill(&record);
        Ok(())
    }

    fn apply_position(&self, record: PositionRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply_position(&record);
        Ok(())
    }

    fn apply_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply_pnl_snapshot(&record);
        Ok(())
    }

    fn apply_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), StateStoreError> {
        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        guard.apply_trade_summary(&record);
        Ok(())
    }

    fn snapshot_history(&self) -> Result<ProjectedTradingHistoryState, StateStoreError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| StateStoreError::Poisoned)?
            .clone())
    }

    fn rebuild_from_records(&self, records: &TradingHistoryRecords) -> Result<(), StateStoreError> {
        let mut projection = ProjectedTradingHistoryState::default();

        for record in &records.strategy_runs {
            projection.apply_strategy_run(record);
        }
        for record in &records.orders {
            projection.apply_order(record);
        }
        for record in &records.fills {
            projection.apply_fill(record);
        }
        for record in &records.positions {
            projection.apply_position(record);
        }
        for record in &records.pnl_snapshots {
            projection.apply_pnl_snapshot(record);
        }
        for record in &records.trade_summaries {
            projection.apply_trade_summary(record);
        }

        let mut guard = self.inner.lock().map_err(|_| StateStoreError::Poisoned)?;
        *guard = projection;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateStoreError {
    #[error("state projection lock is poisoned")]
    Poisoned,
}

fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.to_owned())
}

fn payload_enum<T: for<'de> Deserialize<'de>>(payload: &serde_json::Value, key: &str) -> Option<T> {
    payload
        .get(key)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;
    use tv_bot_core_types::{
        BrokerOrderStatus, EntryOrderType, EventSeverity, RuntimeMode, TradeSide,
    };

    use super::*;

    #[test]
    fn projection_applies_risk_and_execution_events() {
        let mut projection = ProjectedRuntimeState::default();
        let occurred_at = Utc::now();

        projection.apply(&EventJournalRecord {
            event_id: "evt-risk".to_owned(),
            category: "risk".to_owned(),
            action: "hard_override_required".to_owned(),
            source: ActionSource::Cli,
            severity: EventSeverity::Warning,
            occurred_at,
            payload: json!({
                "mode": "paper",
                "strategy_id": "gc_v1",
                "intent": "enter",
                "decision_status": "requires_override",
                "reason": "broker protections missing"
            }),
        });
        projection.apply(&EventJournalRecord {
            event_id: "evt-exec".to_owned(),
            category: "execution".to_owned(),
            action: "dispatch_succeeded".to_owned(),
            source: ActionSource::System,
            severity: EventSeverity::Info,
            occurred_at,
            payload: json!({
                "mode": "paper",
                "strategy_id": "gc_v1",
                "intent": "enter"
            }),
        });

        assert_eq!(projection.total_events, 2);
        assert_eq!(projection.last_strategy_id.as_deref(), Some("gc_v1"));
        assert_eq!(projection.last_mode, Some(RuntimeMode::Paper));
        assert_eq!(
            projection.last_risk_decision_status,
            Some(RiskDecisionStatus::RequiresOverride)
        );
        assert_eq!(projection.hard_override_required_count, 1);
        assert_eq!(projection.dispatch_succeeded_count, 1);
        assert_eq!(projection.manual_event_count, 1);
        assert_eq!(projection.strategy_event_count, 1);
    }

    #[test]
    fn in_memory_state_store_rebuilds_from_events() {
        let store = InMemoryStateStore::new();
        let occurred_at = Utc::now();
        let events = vec![
            EventJournalRecord {
                event_id: "evt-1".to_owned(),
                category: "manual".to_owned(),
                action: "intent_received".to_owned(),
                source: ActionSource::Cli,
                severity: EventSeverity::Info,
                occurred_at,
                payload: json!({
                    "mode": "paper",
                    "strategy_id": "gc_v1",
                    "intent": "flatten"
                }),
            },
            EventJournalRecord {
                event_id: "evt-2".to_owned(),
                category: "execution".to_owned(),
                action: "dispatch_failed".to_owned(),
                source: ActionSource::Cli,
                severity: EventSeverity::Error,
                occurred_at,
                payload: json!({
                    "mode": "paper",
                    "strategy_id": "gc_v1",
                    "intent": "flatten",
                    "error": "broker unavailable"
                }),
            },
        ];

        store
            .rebuild_from_events(&events)
            .expect("rebuild should work");

        let snapshot = store.snapshot().expect("snapshot should work");
        assert_eq!(snapshot.total_events, 2);
        assert_eq!(snapshot.last_error.as_deref(), Some("broker unavailable"));
        assert_eq!(snapshot.dispatch_failed_count, 1);
        assert_eq!(snapshot.last_intent.as_deref(), Some("flatten"));
    }

    #[test]
    fn trading_history_projection_tracks_latest_open_and_closed_state() {
        let store = InMemoryTradingHistoryStore::new();
        let occurred_at = Utc::now();

        store
            .apply_strategy_run(StrategyRunRecord {
                run_id: "run-1".to_owned(),
                strategy_id: "gc_v1".to_owned(),
                mode: RuntimeMode::Paper,
                status: StrategyRunStatus::Active,
                trigger_source: ActionSource::System,
                started_at: occurred_at,
                ended_at: None,
                note: None,
            })
            .expect("active run should apply");
        store
            .apply_strategy_run(StrategyRunRecord {
                run_id: "run-1".to_owned(),
                strategy_id: "gc_v1".to_owned(),
                mode: RuntimeMode::Paper,
                status: StrategyRunStatus::Completed,
                trigger_source: ActionSource::System,
                started_at: occurred_at,
                ended_at: Some(occurred_at + Duration::minutes(5)),
                note: Some("completed".to_owned()),
            })
            .expect("completed run should apply");
        store
            .apply_order(OrderRecord {
                broker_order_id: "ord-1".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                order_type: Some(EntryOrderType::Limit),
                quantity: 1,
                filled_quantity: 0,
                average_fill_price: None,
                status: BrokerOrderStatus::Working,
                provider: "tradovate".to_owned(),
                submitted_at: occurred_at,
                updated_at: occurred_at + Duration::seconds(5),
            })
            .expect("working order should apply");
        store
            .apply_order(OrderRecord {
                broker_order_id: "ord-1".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                order_type: Some(EntryOrderType::Limit),
                quantity: 1,
                filled_quantity: 1,
                average_fill_price: Some(Decimal::new(334550, 2)),
                status: BrokerOrderStatus::Filled,
                provider: "tradovate".to_owned(),
                submitted_at: occurred_at,
                updated_at: occurred_at + Duration::seconds(10),
            })
            .expect("filled order should apply");
        store
            .apply_fill(FillRecord {
                fill_id: "fill-1".to_owned(),
                broker_order_id: Some("ord-1".to_owned()),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                quantity: 1,
                price: Decimal::new(334550, 2),
                fee: Decimal::new(125, 2),
                commission: Decimal::new(75, 2),
                occurred_at: occurred_at + Duration::seconds(11),
            })
            .expect("fill should apply");
        store
            .apply_position(PositionRecord {
                record_id: "pos-open".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                quantity: 1,
                average_price: Some(Decimal::new(334550, 2)),
                realized_pnl: Some(Decimal::ZERO),
                unrealized_pnl: Some(Decimal::new(800, 2)),
                protective_orders_present: true,
                captured_at: occurred_at + Duration::seconds(12),
            })
            .expect("open position should apply");
        store
            .apply_position(PositionRecord {
                record_id: "pos-flat".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                quantity: 0,
                average_price: Some(Decimal::new(334550, 2)),
                realized_pnl: Some(Decimal::new(400, 2)),
                unrealized_pnl: Some(Decimal::ZERO),
                protective_orders_present: false,
                captured_at: occurred_at + Duration::seconds(60),
            })
            .expect("flat position should apply");
        store
            .apply_pnl_snapshot(PnlSnapshotRecord {
                snapshot_id: "pnl-1".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: Some("GCM6".to_owned()),
                gross_pnl: Decimal::new(600, 2),
                net_pnl: Decimal::new(400, 2),
                fees: Decimal::new(125, 2),
                commissions: Decimal::new(75, 2),
                slippage: Decimal::new(0, 0),
                realized_pnl: Some(Decimal::new(400, 2)),
                unrealized_pnl: Some(Decimal::ZERO),
                captured_at: occurred_at + Duration::seconds(61),
            })
            .expect("pnl snapshot should apply");
        store
            .apply_trade_summary(TradeSummaryRecord {
                trade_id: "trade-1".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-1".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                side: TradeSide::Buy,
                status: TradeSummaryStatus::Closed,
                quantity: 1,
                average_entry_price: Decimal::new(334550, 2),
                average_exit_price: Some(Decimal::new(335150, 2)),
                opened_at: occurred_at,
                closed_at: Some(occurred_at + Duration::seconds(62)),
                gross_pnl: Decimal::new(600, 2),
                net_pnl: Decimal::new(400, 2),
                fees: Decimal::new(125, 2),
                commissions: Decimal::new(75, 2),
                slippage: Decimal::new(0, 0),
            })
            .expect("trade summary should apply");

        let snapshot = store.snapshot_history().expect("snapshot should work");
        assert_eq!(snapshot.total_strategy_run_records, 2);
        assert!(snapshot.active_run_ids.is_empty());
        assert_eq!(snapshot.total_order_records, 2);
        assert!(snapshot.working_order_ids.is_empty());
        assert_eq!(snapshot.total_fill_records, 1);
        assert_eq!(snapshot.recorded_fill_fees, Decimal::new(125, 2));
        assert_eq!(snapshot.recorded_fill_commissions, Decimal::new(75, 2));
        assert_eq!(snapshot.total_position_records, 2);
        assert!(snapshot.open_position_symbols.is_empty());
        assert_eq!(snapshot.total_pnl_snapshot_records, 1);
        assert_eq!(
            snapshot
                .latest_pnl_snapshot
                .as_ref()
                .map(|record| record.net_pnl),
            Some(Decimal::new(400, 2))
        );
        assert_eq!(snapshot.total_trade_summary_records, 1);
        assert_eq!(snapshot.closed_trade_count, 1);
        assert_eq!(snapshot.closed_trade_gross_pnl, Decimal::new(600, 2));
        assert_eq!(snapshot.closed_trade_net_pnl, Decimal::new(400, 2));
    }

    #[test]
    fn trading_history_store_rebuilds_from_record_sets() {
        let store = InMemoryTradingHistoryStore::new();
        let occurred_at = Utc::now();
        let records = TradingHistoryRecords {
            strategy_runs: vec![StrategyRunRecord {
                run_id: "run-2".to_owned(),
                strategy_id: "gc_v1".to_owned(),
                mode: RuntimeMode::Paper,
                status: StrategyRunStatus::Paused,
                trigger_source: ActionSource::Cli,
                started_at: occurred_at,
                ended_at: None,
                note: Some("paused".to_owned()),
            }],
            orders: vec![OrderRecord {
                broker_order_id: "ord-2".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-2".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                side: TradeSide::Sell,
                order_type: Some(EntryOrderType::Market),
                quantity: 1,
                filled_quantity: 0,
                average_fill_price: None,
                status: BrokerOrderStatus::Pending,
                provider: "tradovate".to_owned(),
                submitted_at: occurred_at,
                updated_at: occurred_at + Duration::seconds(1),
            }],
            fills: Vec::new(),
            positions: vec![PositionRecord {
                record_id: "pos-2".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-2".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                quantity: -1,
                average_price: Some(Decimal::new(334400, 2)),
                realized_pnl: Some(Decimal::ZERO),
                unrealized_pnl: Some(Decimal::new(-250, 2)),
                protective_orders_present: true,
                captured_at: occurred_at + Duration::seconds(2),
            }],
            pnl_snapshots: Vec::new(),
            trade_summaries: vec![TradeSummaryRecord {
                trade_id: "trade-open".to_owned(),
                strategy_id: Some("gc_v1".to_owned()),
                run_id: Some("run-2".to_owned()),
                account_id: Some("acct-paper".to_owned()),
                symbol: "GCM6".to_owned(),
                side: TradeSide::Sell,
                status: TradeSummaryStatus::Open,
                quantity: 1,
                average_entry_price: Decimal::new(334400, 2),
                average_exit_price: None,
                opened_at: occurred_at + Duration::seconds(1),
                closed_at: None,
                gross_pnl: Decimal::ZERO,
                net_pnl: Decimal::ZERO,
                fees: Decimal::ZERO,
                commissions: Decimal::ZERO,
                slippage: Decimal::ZERO,
            }],
        };

        store
            .rebuild_from_records(&records)
            .expect("rebuild should work");

        let snapshot = store.snapshot_history().expect("snapshot should work");
        assert_eq!(snapshot.active_run_ids, vec!["run-2".to_owned()]);
        assert_eq!(snapshot.working_order_ids, vec!["ord-2".to_owned()]);
        assert_eq!(snapshot.open_position_symbols, vec!["GCM6".to_owned()]);
        assert_eq!(snapshot.open_trade_ids, vec!["trade-open".to_owned()]);
    }
}
