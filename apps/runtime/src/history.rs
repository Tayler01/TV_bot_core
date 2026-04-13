use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use thiserror::Error;
use tv_bot_control_api::{RuntimeHistorySnapshot, RuntimeReconnectDecision};
use tv_bot_core_types::{
    ActionSource, BrokerAccountSnapshot, BrokerOrderStatus, BrokerOrderUpdate,
    BrokerPositionSnapshot, ExecutionIntent, FillRecord, OrderRecord, PnlSnapshotRecord,
    PositionRecord, RuntimeMode, StrategyRunRecord, StrategyRunStatus, TradeSide,
    TradeSummaryRecord, TradeSummaryStatus,
};
use tv_bot_execution_engine::ExecutionDispatchResult;
use tv_bot_persistence::{
    FillStore, OrderStore, PersistenceError, PnlSnapshotStore, PositionStore, RuntimePersistence,
    StrategyRunStore, TradeSummaryStore,
};
use tv_bot_runtime_kernel::{RuntimeCommand, RuntimeCommandOutcome, RuntimeExecutionRequest};
use tv_bot_state_store::{
    InMemoryTradingHistoryStore, ProjectedTradingHistoryState, StateStoreError,
    TradingHistoryProjectionStore, TradingHistoryRecords,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeBrokerSnapshot {
    pub broker_status: Option<tv_bot_core_types::BrokerStatusSnapshot>,
    pub last_reconnect_review_decision: Option<RuntimeReconnectDecision>,
    pub account_snapshot: Option<BrokerAccountSnapshot>,
    pub open_positions: Vec<BrokerPositionSnapshot>,
    pub working_orders: Vec<BrokerOrderUpdate>,
    pub fills: Vec<tv_bot_core_types::BrokerFillUpdate>,
}

#[derive(Clone)]
pub struct RuntimeHistoryRecorder {
    state: Arc<Mutex<RuntimeHistoryState>>,
    projection: InMemoryTradingHistoryStore,
    strategy_run_store: Arc<dyn StrategyRunStore>,
    order_store: Arc<dyn OrderStore>,
    fill_store: Arc<dyn FillStore>,
    position_store: Arc<dyn PositionStore>,
    pnl_snapshot_store: Arc<dyn PnlSnapshotStore>,
    trade_summary_store: Arc<dyn TradeSummaryStore>,
}

#[derive(Clone, Debug, Default)]
struct RuntimeHistoryState {
    current_run: Option<ActiveRunState>,
    last_orders: BTreeMap<String, BrokerOrderUpdate>,
    seen_fills: BTreeSet<String>,
    last_positions: BTreeMap<String, BrokerPositionSnapshot>,
    last_account_snapshot: Option<BrokerAccountSnapshot>,
}

#[derive(Clone, Debug)]
struct ActiveRunState {
    run_id: String,
    strategy_id: String,
    mode: RuntimeMode,
    status: StrategyRunStatus,
    trigger_source: ActionSource,
    started_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum RuntimeHistoryError {
    #[error("history persistence failed: {source}")]
    Persistence {
        #[source]
        source: PersistenceError,
    },
    #[error("history projection failed: {source}")]
    StateStore {
        #[source]
        source: StateStoreError,
    },
    #[error("history recorder lock is poisoned")]
    Poisoned,
}

impl RuntimeHistoryRecorder {
    pub fn from_persistence(persistence: &RuntimePersistence) -> Result<Self, RuntimeHistoryError> {
        let projection = InMemoryTradingHistoryStore::new();
        let strategy_run_store = persistence.strategy_run_store();
        let order_store = persistence.order_store();
        let fill_store = persistence.fill_store();
        let position_store = persistence.position_store();
        let pnl_snapshot_store = persistence.pnl_snapshot_store();
        let trade_summary_store = persistence.trade_summary_store();

        let records = TradingHistoryRecords {
            strategy_runs: strategy_run_store
                .list_strategy_runs()
                .map_err(|source| RuntimeHistoryError::Persistence { source })?,
            orders: order_store
                .list_orders()
                .map_err(|source| RuntimeHistoryError::Persistence { source })?,
            fills: fill_store
                .list_fills()
                .map_err(|source| RuntimeHistoryError::Persistence { source })?,
            positions: position_store
                .list_positions()
                .map_err(|source| RuntimeHistoryError::Persistence { source })?,
            pnl_snapshots: pnl_snapshot_store
                .list_pnl_snapshots()
                .map_err(|source| RuntimeHistoryError::Persistence { source })?,
            trade_summaries: trade_summary_store
                .list_trade_summaries()
                .map_err(|source| RuntimeHistoryError::Persistence { source })?,
        };
        projection
            .rebuild_from_records(&records)
            .map_err(|source| RuntimeHistoryError::StateStore { source })?;

        let snapshot = projection
            .snapshot_history()
            .map_err(|source| RuntimeHistoryError::StateStore { source })?;
        let current_run = snapshot
            .latest_run
            .filter(|run| {
                matches!(
                    run.status,
                    StrategyRunStatus::Starting
                        | StrategyRunStatus::Active
                        | StrategyRunStatus::Paused
                        | StrategyRunStatus::Failed
                )
            })
            .map(|run| ActiveRunState {
                run_id: run.run_id,
                strategy_id: run.strategy_id,
                mode: run.mode,
                status: run.status,
                trigger_source: run.trigger_source,
                started_at: run.started_at,
            });

        Ok(Self {
            state: Arc::new(Mutex::new(RuntimeHistoryState {
                current_run,
                ..RuntimeHistoryState::default()
            })),
            projection,
            strategy_run_store,
            order_store,
            fill_store,
            position_store,
            pnl_snapshot_store,
            trade_summary_store,
        })
    }

    pub fn snapshot(&self) -> Result<RuntimeHistorySnapshot, RuntimeHistoryError> {
        Ok(RuntimeHistorySnapshot {
            projection: self
                .projection
                .snapshot_history()
                .map_err(|source| RuntimeHistoryError::StateStore { source })?,
        })
    }

    pub fn record_strategy_loaded(
        &self,
        strategy_id: String,
        mode: RuntimeMode,
        source: ActionSource,
        occurred_at: DateTime<Utc>,
    ) -> Result<Option<RuntimeHistorySnapshot>, RuntimeHistoryError> {
        let mut state = self.lock_state()?;

        if let Some(previous) = state.current_run.take() {
            self.persist_strategy_run(strategy_run_record(
                &previous,
                StrategyRunStatus::Cancelled,
                Some(occurred_at),
            ))?;
        }

        let current = ActiveRunState {
            run_id: format!(
                "run-{}-{}",
                strategy_id.replace(' ', "_"),
                occurred_at.timestamp_nanos_opt().unwrap_or_default()
            ),
            strategy_id,
            mode,
            status: StrategyRunStatus::Starting,
            trigger_source: source,
            started_at: occurred_at,
        };
        self.persist_strategy_run(strategy_run_record(
            &current,
            StrategyRunStatus::Starting,
            None,
        ))?;
        state.current_run = Some(current);

        snapshot_if_changed(self, true)
    }

    pub fn record_mode_change(
        &self,
        mode: RuntimeMode,
    ) -> Result<Option<RuntimeHistorySnapshot>, RuntimeHistoryError> {
        let mut state = self.lock_state()?;
        let Some(current) = state.current_run.as_mut() else {
            return Ok(None);
        };

        if current.mode == mode {
            return Ok(None);
        }

        current.mode = mode;
        let status = current.status.clone();
        self.persist_strategy_run(strategy_run_record(current, status, None))?;
        snapshot_if_changed(self, true)
    }

    pub fn record_run_status(
        &self,
        status: StrategyRunStatus,
        occurred_at: DateTime<Utc>,
    ) -> Result<Option<RuntimeHistorySnapshot>, RuntimeHistoryError> {
        let mut state = self.lock_state()?;
        let Some(current) = state.current_run.as_mut() else {
            return Ok(None);
        };

        current.status = status;
        let current_status = current.status.clone();
        self.persist_strategy_run(strategy_run_record(
            current,
            current_status.clone(),
            terminal_time(current_status, occurred_at),
        ))?;
        snapshot_if_changed(self, true)
    }

    pub fn record_execution_outcome(
        &self,
        command: &RuntimeCommand,
        outcome: &RuntimeCommandOutcome,
        snapshot: &RuntimeBrokerSnapshot,
        occurred_at: DateTime<Utc>,
    ) -> Result<Option<RuntimeHistorySnapshot>, RuntimeHistoryError> {
        let request = request_for_command(command);
        let RuntimeCommandOutcome::Execution(outcome) = outcome;
        let Some((side, quantity, order_type)) = order_details_for_request(request) else {
            return Ok(None);
        };

        let account_id = snapshot
            .broker_status
            .as_ref()
            .and_then(|status| status.selected_account.as_ref())
            .map(|selection| selection.account_id.clone())
            .or_else(|| {
                snapshot
                    .account_snapshot
                    .as_ref()
                    .map(|account| account.account_id.clone())
            });
        let (_, run_id) = self.current_run_identifiers()?;

        let mut changed = false;
        if let Some(dispatch) = &outcome.dispatch {
            for result in &dispatch.results {
                match result {
                    ExecutionDispatchResult::OrderSubmitted {
                        order_id, symbol, ..
                    } => {
                        self.persist_order(OrderRecord {
                            broker_order_id: order_id.to_string(),
                            strategy_id: Some(
                                request.execution.strategy.metadata.strategy_id.clone(),
                            ),
                            run_id: run_id.clone(),
                            account_id: account_id.clone(),
                            symbol: symbol.clone(),
                            side,
                            order_type: Some(order_type),
                            quantity,
                            filled_quantity: 0,
                            average_fill_price: None,
                            status: BrokerOrderStatus::Pending,
                            provider: "tradovate".to_owned(),
                            submitted_at: occurred_at,
                            updated_at: occurred_at,
                        })?;
                        changed = true;
                    }
                    ExecutionDispatchResult::PositionLiquidated { order_id, .. } => {
                        self.persist_order(OrderRecord {
                            broker_order_id: order_id.to_string(),
                            strategy_id: Some(
                                request.execution.strategy.metadata.strategy_id.clone(),
                            ),
                            run_id: run_id.clone(),
                            account_id: account_id.clone(),
                            symbol: request.execution.instrument.tradovate_symbol.clone(),
                            side,
                            order_type: Some(order_type),
                            quantity,
                            filled_quantity: 0,
                            average_fill_price: None,
                            status: BrokerOrderStatus::Pending,
                            provider: "tradovate".to_owned(),
                            submitted_at: occurred_at,
                            updated_at: occurred_at,
                        })?;
                        changed = true;
                    }
                    ExecutionDispatchResult::OrderCancelled { .. } => {}
                    ExecutionDispatchResult::StrategyPaused { .. } => {}
                }
            }
        }

        snapshot_if_changed(self, changed)
    }

    pub fn sync_broker_snapshot(
        &self,
        snapshot: &RuntimeBrokerSnapshot,
        occurred_at: DateTime<Utc>,
    ) -> Result<Option<RuntimeHistorySnapshot>, RuntimeHistoryError> {
        let default_account_id = snapshot
            .broker_status
            .as_ref()
            .and_then(|status| status.selected_account.as_ref())
            .map(|selection| selection.account_id.clone())
            .or_else(|| {
                snapshot
                    .account_snapshot
                    .as_ref()
                    .map(|account| account.account_id.clone())
            });
        let history_snapshot = self
            .projection
            .snapshot_history()
            .map_err(|source| RuntimeHistoryError::StateStore { source })?;
        let mut state = self.lock_state()?;
        let strategy_id = state
            .current_run
            .as_ref()
            .map(|run| run.strategy_id.clone());
        let run_id = state.current_run.as_ref().map(|run| run.run_id.clone());
        let mut changed = false;
        let mut order_records = Vec::new();
        let mut fill_records = Vec::new();
        let mut position_records = Vec::new();
        let mut trade_summary_positions = Vec::new();
        let mut new_fill_quantities = BTreeMap::<String, u32>::new();
        let mut account_snapshot_for_pnl = None;

        let current_order_ids = snapshot
            .working_orders
            .iter()
            .map(|order| order.broker_order_id.clone())
            .collect::<BTreeSet<_>>();

        for order in &snapshot.working_orders {
            let previous = state.last_orders.get(&order.broker_order_id);
            if previous == Some(order) {
                continue;
            }

            if let Some(record) = merge_order_record(
                &history_snapshot,
                order,
                strategy_id.clone(),
                run_id.clone(),
                default_account_id.clone(),
            ) {
                order_records.push(record);
                changed = true;
            }
            state
                .last_orders
                .insert(order.broker_order_id.clone(), order.clone());
        }

        let removed_orders = state
            .last_orders
            .keys()
            .filter(|order_id| !current_order_ids.contains(*order_id))
            .cloned()
            .collect::<Vec<_>>();

        for fill in &snapshot.fills {
            if !state.seen_fills.insert(fill.fill_id.clone()) {
                continue;
            }

            fill_records.push(FillRecord {
                fill_id: fill.fill_id.clone(),
                broker_order_id: fill.broker_order_id.clone(),
                strategy_id: strategy_id.clone(),
                run_id: run_id.clone(),
                account_id: fill.account_id.clone().or(default_account_id.clone()),
                symbol: fill.symbol.clone(),
                side: fill.side,
                quantity: fill.quantity,
                price: fill.price,
                fee: fill.fee.unwrap_or(Decimal::ZERO),
                commission: fill.commission.unwrap_or(Decimal::ZERO),
                occurred_at: fill.occurred_at,
            });
            if let Some(order_id) = fill.broker_order_id.clone() {
                *new_fill_quantities.entry(order_id).or_default() += fill.quantity;
            }
            changed = true;
        }

        let current_position_keys = snapshot
            .open_positions
            .iter()
            .map(position_key)
            .collect::<BTreeSet<_>>();
        for position in &snapshot.open_positions {
            let key = position_key(position);
            let previous = state.last_positions.get(&key);
            if previous == Some(position) {
                continue;
            }

            position_records.push(build_position_record(
                position,
                strategy_id.clone(),
                run_id.clone(),
                occurred_at,
            ));
            trade_summary_positions.push(position.clone());
            state.last_positions.insert(key, position.clone());
            changed = true;
        }

        let removed_positions = state
            .last_positions
            .keys()
            .filter(|key| !current_position_keys.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        for key in removed_positions {
            if let Some(previous) = state.last_positions.remove(&key) {
                let flattened = BrokerPositionSnapshot {
                    account_id: previous.account_id.clone(),
                    symbol: previous.symbol.clone(),
                    quantity: 0,
                    average_price: previous.average_price,
                    realized_pnl: previous.realized_pnl,
                    unrealized_pnl: Some(Decimal::ZERO),
                    protective_orders_present: false,
                    captured_at: occurred_at,
                };
                position_records.push(build_position_record(
                    &flattened,
                    strategy_id.clone(),
                    run_id.clone(),
                    occurred_at,
                ));
                trade_summary_positions.push(flattened);
                changed = true;
            }
        }

        if state.last_account_snapshot.as_ref() != snapshot.account_snapshot.as_ref() || changed {
            if let Some(account_snapshot) = snapshot.account_snapshot.clone() {
                state.last_account_snapshot = Some(account_snapshot);
                account_snapshot_for_pnl = snapshot.account_snapshot.clone();
                changed = true;
            }
        }

        for order_id in removed_orders {
            if let Some(previous) = state.last_orders.remove(&order_id) {
                if let Some(existing) = history_snapshot.orders.get(&order_id) {
                    let filled_quantity = total_filled_for_order(&history_snapshot, &order_id)
                        + new_fill_quantities.get(&order_id).copied().unwrap_or(0);
                    let final_status = if filled_quantity >= existing.quantity {
                        BrokerOrderStatus::Filled
                    } else {
                        BrokerOrderStatus::Cancelled
                    };
                    order_records.push(OrderRecord {
                        broker_order_id: existing.broker_order_id.clone(),
                        strategy_id: existing.strategy_id.clone(),
                        run_id: existing.run_id.clone(),
                        account_id: existing.account_id.clone(),
                        symbol: existing.symbol.clone(),
                        side: existing.side,
                        order_type: existing.order_type,
                        quantity: existing.quantity,
                        filled_quantity,
                        average_fill_price: previous
                            .average_fill_price
                            .or(existing.average_fill_price),
                        status: final_status,
                        provider: existing.provider.clone(),
                        submitted_at: existing.submitted_at,
                        updated_at: occurred_at,
                    });
                    changed = true;
                }
            }
        }

        drop(state);

        for order_record in order_records {
            self.persist_order(order_record)?;
        }
        for fill_record in fill_records {
            self.persist_fill(fill_record)?;
        }
        for position_record in position_records {
            self.persist_position(position_record)?;
        }
        for position in trade_summary_positions {
            self.sync_trade_summary_for_position(&position, occurred_at)?;
        }
        if let Some(account_snapshot) = account_snapshot_for_pnl {
            let costs = self
                .projection
                .snapshot_history()
                .map_err(|source| RuntimeHistoryError::StateStore { source })?;
            self.persist_pnl_snapshot(build_pnl_snapshot_record(
                &account_snapshot,
                strategy_id,
                run_id,
                &costs,
                occurred_at,
            ))?;
        }

        snapshot_if_changed(self, changed)
    }

    fn current_run_identifiers(
        &self,
    ) -> Result<(Option<String>, Option<String>), RuntimeHistoryError> {
        let state = self.lock_state()?;
        Ok(match &state.current_run {
            Some(run) => (Some(run.strategy_id.clone()), Some(run.run_id.clone())),
            None => (None, None),
        })
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, RuntimeHistoryState>, RuntimeHistoryError> {
        self.state.lock().map_err(|_| RuntimeHistoryError::Poisoned)
    }

    fn persist_strategy_run(&self, record: StrategyRunRecord) -> Result<(), RuntimeHistoryError> {
        self.strategy_run_store
            .append_strategy_run(record.clone())
            .map_err(|source| RuntimeHistoryError::Persistence { source })?;
        self.projection
            .apply_strategy_run(record)
            .map_err(|source| RuntimeHistoryError::StateStore { source })
    }

    fn persist_order(&self, record: OrderRecord) -> Result<(), RuntimeHistoryError> {
        self.order_store
            .append_order(record.clone())
            .map_err(|source| RuntimeHistoryError::Persistence { source })?;
        self.projection
            .apply_order(record)
            .map_err(|source| RuntimeHistoryError::StateStore { source })
    }

    fn persist_fill(&self, record: FillRecord) -> Result<(), RuntimeHistoryError> {
        self.fill_store
            .append_fill(record.clone())
            .map_err(|source| RuntimeHistoryError::Persistence { source })?;
        self.projection
            .apply_fill(record)
            .map_err(|source| RuntimeHistoryError::StateStore { source })
    }

    fn persist_position(&self, record: PositionRecord) -> Result<(), RuntimeHistoryError> {
        self.position_store
            .append_position(record.clone())
            .map_err(|source| RuntimeHistoryError::Persistence { source })?;
        self.projection
            .apply_position(record)
            .map_err(|source| RuntimeHistoryError::StateStore { source })
    }

    fn persist_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), RuntimeHistoryError> {
        self.pnl_snapshot_store
            .append_pnl_snapshot(record.clone())
            .map_err(|source| RuntimeHistoryError::Persistence { source })?;
        self.projection
            .apply_pnl_snapshot(record)
            .map_err(|source| RuntimeHistoryError::StateStore { source })
    }

    fn persist_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), RuntimeHistoryError> {
        self.trade_summary_store
            .append_trade_summary(record.clone())
            .map_err(|source| RuntimeHistoryError::Persistence { source })?;
        self.projection
            .apply_trade_summary(record)
            .map_err(|source| RuntimeHistoryError::StateStore { source })
    }

    fn sync_trade_summary_for_position(
        &self,
        position: &BrokerPositionSnapshot,
        occurred_at: DateTime<Utc>,
    ) -> Result<(), RuntimeHistoryError> {
        let snapshot = self
            .projection
            .snapshot_history()
            .map_err(|source| RuntimeHistoryError::StateStore { source })?;
        let existing = snapshot
            .trade_summaries
            .values()
            .find(|record| {
                record.status == TradeSummaryStatus::Open
                    && record.symbol.eq_ignore_ascii_case(&position.symbol)
            })
            .cloned();

        let position_side = if position.quantity > 0 {
            Some(TradeSide::Buy)
        } else if position.quantity < 0 {
            Some(TradeSide::Sell)
        } else {
            None
        };

        match (existing, position_side) {
            (None, Some(side)) => {
                let (strategy_id, run_id) = self.current_run_identifiers()?;
                self.persist_trade_summary(TradeSummaryRecord {
                    trade_id: format!(
                        "trade-{}-{}",
                        position.symbol,
                        occurred_at.timestamp_nanos_opt().unwrap_or_default()
                    ),
                    strategy_id,
                    run_id,
                    account_id: position.account_id.clone(),
                    symbol: position.symbol.clone(),
                    side,
                    status: TradeSummaryStatus::Open,
                    quantity: position.quantity.unsigned_abs(),
                    average_entry_price: position.average_price.unwrap_or(Decimal::ZERO),
                    average_exit_price: None,
                    opened_at: position.captured_at,
                    closed_at: None,
                    gross_pnl: Decimal::ZERO,
                    net_pnl: Decimal::ZERO,
                    fees: Decimal::ZERO,
                    commissions: Decimal::ZERO,
                    slippage: Decimal::ZERO,
                })?;
            }
            (Some(mut record), Some(side)) if record.side == side => {
                record.quantity = position.quantity.unsigned_abs();
                if let Some(price) = position.average_price {
                    record.average_entry_price = price;
                }
                self.persist_trade_summary(record)?;
            }
            (Some(record), Some(_)) => {
                self.close_trade_summary(record, position, occurred_at)?;
                self.sync_trade_summary_for_position(position, occurred_at)?;
            }
            (Some(record), None) => {
                self.close_trade_summary(record, position, occurred_at)?;
            }
            (None, None) => {}
        }

        Ok(())
    }

    fn close_trade_summary(
        &self,
        mut record: TradeSummaryRecord,
        position: &BrokerPositionSnapshot,
        occurred_at: DateTime<Utc>,
    ) -> Result<(), RuntimeHistoryError> {
        let snapshot = self
            .projection
            .snapshot_history()
            .map_err(|source| RuntimeHistoryError::StateStore { source })?;
        let fills = snapshot
            .fills
            .values()
            .filter(|fill| {
                fill.symbol.eq_ignore_ascii_case(&record.symbol)
                    && fill.occurred_at >= record.opened_at
                    && fill.occurred_at <= occurred_at
            })
            .cloned()
            .collect::<Vec<_>>();
        let fees = fills.iter().fold(Decimal::ZERO, |sum, fill| sum + fill.fee);
        let commissions = fills
            .iter()
            .fold(Decimal::ZERO, |sum, fill| sum + fill.commission);
        let exit_price = fills
            .iter()
            .max_by_key(|fill| fill.occurred_at)
            .map(|fill| fill.price);
        let gross_pnl = position.realized_pnl.unwrap_or(Decimal::ZERO);

        record.status = TradeSummaryStatus::Closed;
        record.closed_at = Some(occurred_at);
        record.average_exit_price = exit_price;
        record.gross_pnl = gross_pnl;
        record.fees = fees;
        record.commissions = commissions;
        record.slippage = Decimal::ZERO;
        record.net_pnl = gross_pnl - fees - commissions;

        self.persist_trade_summary(record)
    }
}

fn snapshot_if_changed(
    recorder: &RuntimeHistoryRecorder,
    changed: bool,
) -> Result<Option<RuntimeHistorySnapshot>, RuntimeHistoryError> {
    if changed {
        recorder.snapshot().map(Some)
    } else {
        Ok(None)
    }
}

fn request_for_command(command: &RuntimeCommand) -> &RuntimeExecutionRequest {
    match command {
        RuntimeCommand::ManualIntent(request) | RuntimeCommand::StrategyIntent(request) => request,
    }
}

fn order_details_for_request(
    request: &RuntimeExecutionRequest,
) -> Option<(TradeSide, u32, tv_bot_core_types::EntryOrderType)> {
    match &request.execution.intent {
        ExecutionIntent::Enter {
            side,
            quantity,
            order_type,
            ..
        } => Some((*side, *quantity, *order_type)),
        ExecutionIntent::ReducePosition { quantity, .. } => request
            .execution
            .state
            .current_position
            .as_ref()
            .and_then(|position| opposite_side_for_quantity(position.quantity))
            .map(|side| (side, *quantity, tv_bot_core_types::EntryOrderType::Market)),
        ExecutionIntent::Exit { .. } | ExecutionIntent::Flatten { .. } => request
            .execution
            .state
            .current_position
            .as_ref()
            .and_then(|position| opposite_side_for_quantity(position.quantity))
            .map(|side| {
                (
                    side,
                    request
                        .execution
                        .state
                        .current_position
                        .as_ref()
                        .map(|position| position.quantity.unsigned_abs())
                        .unwrap_or(0),
                    tv_bot_core_types::EntryOrderType::Market,
                )
            }),
        ExecutionIntent::CancelWorkingOrders { .. } | ExecutionIntent::PauseStrategy { .. } => None,
    }
}

fn opposite_side_for_quantity(quantity: i32) -> Option<TradeSide> {
    if quantity > 0 {
        Some(TradeSide::Sell)
    } else if quantity < 0 {
        Some(TradeSide::Buy)
    } else {
        None
    }
}

fn strategy_run_record(
    state: &ActiveRunState,
    status: StrategyRunStatus,
    ended_at: Option<DateTime<Utc>>,
) -> StrategyRunRecord {
    StrategyRunRecord {
        run_id: state.run_id.clone(),
        strategy_id: state.strategy_id.clone(),
        mode: state.mode.clone(),
        status,
        trigger_source: state.trigger_source,
        started_at: state.started_at,
        ended_at,
        note: None,
    }
}

fn terminal_time(status: StrategyRunStatus, occurred_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if matches!(
        status,
        StrategyRunStatus::Completed | StrategyRunStatus::Failed | StrategyRunStatus::Cancelled
    ) {
        Some(occurred_at)
    } else {
        None
    }
}

fn merge_order_record(
    snapshot: &ProjectedTradingHistoryState,
    order: &BrokerOrderUpdate,
    strategy_id: Option<String>,
    run_id: Option<String>,
    default_account_id: Option<String>,
) -> Option<OrderRecord> {
    let existing = snapshot.orders.get(&order.broker_order_id);
    let side = order.side.or(existing.map(|record| record.side))?;
    let quantity = order.quantity.or(existing.map(|record| record.quantity))?;

    Some(OrderRecord {
        broker_order_id: order.broker_order_id.clone(),
        strategy_id: existing
            .and_then(|record| record.strategy_id.clone())
            .or(strategy_id),
        run_id: existing.and_then(|record| record.run_id.clone()).or(run_id),
        account_id: order
            .account_id
            .clone()
            .or_else(|| existing.and_then(|record| record.account_id.clone()))
            .or(default_account_id),
        symbol: order.symbol.clone(),
        side,
        order_type: order
            .order_type
            .or(existing.and_then(|record| record.order_type)),
        quantity,
        filled_quantity: order.filled_quantity,
        average_fill_price: order
            .average_fill_price
            .or(existing.and_then(|record| record.average_fill_price)),
        status: order.status,
        provider: existing
            .map(|record| record.provider.clone())
            .unwrap_or_else(|| "tradovate".to_owned()),
        submitted_at: existing
            .map(|record| record.submitted_at)
            .unwrap_or(order.updated_at),
        updated_at: order.updated_at,
    })
}

fn build_position_record(
    position: &BrokerPositionSnapshot,
    strategy_id: Option<String>,
    run_id: Option<String>,
    occurred_at: DateTime<Utc>,
) -> PositionRecord {
    PositionRecord {
        record_id: format!(
            "position-{}-{}-{}",
            position.account_id.as_deref().unwrap_or("unknown"),
            position.symbol,
            occurred_at.timestamp_nanos_opt().unwrap_or_default()
        ),
        strategy_id,
        run_id,
        account_id: position.account_id.clone(),
        symbol: position.symbol.clone(),
        quantity: position.quantity,
        average_price: position.average_price,
        realized_pnl: position.realized_pnl,
        unrealized_pnl: position.unrealized_pnl,
        protective_orders_present: position.protective_orders_present,
        captured_at: occurred_at,
    }
}

fn build_pnl_snapshot_record(
    account: &BrokerAccountSnapshot,
    strategy_id: Option<String>,
    run_id: Option<String>,
    snapshot: &ProjectedTradingHistoryState,
    occurred_at: DateTime<Utc>,
) -> PnlSnapshotRecord {
    let realized = account.realized_pnl.unwrap_or(Decimal::ZERO);
    let unrealized = account.unrealized_pnl.unwrap_or(Decimal::ZERO);
    let gross_pnl = realized + unrealized;
    let fees = snapshot.recorded_fill_fees.max(snapshot.closed_trade_fees);
    let commissions = snapshot
        .recorded_fill_commissions
        .max(snapshot.closed_trade_commissions);
    let slippage = snapshot.closed_trade_slippage;

    PnlSnapshotRecord {
        snapshot_id: format!(
            "pnl-{}",
            occurred_at.timestamp_nanos_opt().unwrap_or_default()
        ),
        strategy_id,
        run_id,
        account_id: Some(account.account_id.clone()),
        symbol: None,
        gross_pnl,
        net_pnl: gross_pnl - fees - commissions - slippage,
        fees,
        commissions,
        slippage,
        realized_pnl: account.realized_pnl,
        unrealized_pnl: account.unrealized_pnl,
        captured_at: occurred_at,
    }
}

fn total_filled_for_order(snapshot: &ProjectedTradingHistoryState, order_id: &str) -> u32 {
    snapshot
        .fills
        .values()
        .filter(|fill| fill.broker_order_id.as_deref() == Some(order_id))
        .map(|fill| fill.quantity)
        .sum()
}

fn position_key(position: &BrokerPositionSnapshot) -> String {
    format!(
        "{}:{}",
        position.account_id.as_deref().unwrap_or("unknown"),
        position.symbol
    )
}

#[cfg(test)]
mod tests {
    use tv_bot_config::{AppConfig, MapEnvironment};
    use tv_bot_core_types::{
        BrokerAccountRouting, BrokerConnectionState, BrokerEnvironment, BrokerFillUpdate,
        BrokerHealth, BrokerOrderStatus, BrokerStatusSnapshot, BrokerSyncState, EntryOrderType,
        StrategyRunStatus,
    };
    use tv_bot_persistence::RuntimePersistence;

    use super::*;

    fn test_recorder() -> RuntimeHistoryRecorder {
        let config = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [runtime]
                startup_mode = "observation"

                [control_api]
                http_bind = "127.0.0.1:8080"
                websocket_bind = "127.0.0.1:8081"
            "#,
            &MapEnvironment::default(),
        )
        .expect("config should load");
        let persistence = RuntimePersistence::open(&config);

        RuntimeHistoryRecorder::from_persistence(&persistence)
            .expect("history recorder should initialize")
    }

    fn sample_broker_snapshot(
        position_quantity: i32,
        include_working_order: bool,
        include_fill: bool,
    ) -> RuntimeBrokerSnapshot {
        RuntimeBrokerSnapshot {
            broker_status: Some(BrokerStatusSnapshot {
                provider: "tradovate".to_owned(),
                environment: BrokerEnvironment::Demo,
                connection_state: BrokerConnectionState::Connected,
                health: BrokerHealth::Healthy,
                sync_state: BrokerSyncState::Synchronized,
                selected_account: Some(tv_bot_core_types::BrokerAccountSelection {
                    provider: "tradovate".to_owned(),
                    account_id: "acct-1".to_owned(),
                    account_name: "paper-primary".to_owned(),
                    routing: BrokerAccountRouting::Paper,
                    environment: BrokerEnvironment::Demo,
                    selected_at: Utc::now(),
                }),
                reconnect_count: 0,
                last_authenticated_at: Some(Utc::now()),
                last_heartbeat_at: Some(Utc::now()),
                last_sync_at: Some(Utc::now()),
                last_disconnect_reason: None,
                review_required_reason: None,
                updated_at: Utc::now(),
            }),
            last_reconnect_review_decision: None,
            account_snapshot: Some(BrokerAccountSnapshot {
                account_id: "acct-1".to_owned(),
                account_name: Some("paper-primary".to_owned()),
                cash_balance: Some(Decimal::new(50_000, 0)),
                available_funds: Some(Decimal::new(49_750, 0)),
                excess_liquidity: Some(Decimal::new(49_750, 0)),
                margin_used: Some(Decimal::new(250, 0)),
                net_liquidation_value: Some(Decimal::new(50_250, 0)),
                realized_pnl: Some(Decimal::new(250, 0)),
                unrealized_pnl: Some(Decimal::new(125, 0)),
                risk_state: Some("healthy".to_owned()),
                captured_at: Utc::now(),
            }),
            open_positions: (position_quantity != 0)
                .then_some(BrokerPositionSnapshot {
                    account_id: Some("acct-1".to_owned()),
                    symbol: "GCM2026".to_owned(),
                    quantity: position_quantity,
                    average_price: Some(Decimal::new(238_500, 2)),
                    realized_pnl: Some(Decimal::new(250, 0)),
                    unrealized_pnl: Some(Decimal::new(125, 0)),
                    protective_orders_present: true,
                    captured_at: Utc::now(),
                })
                .into_iter()
                .collect(),
            working_orders: include_working_order
                .then_some(BrokerOrderUpdate {
                    broker_order_id: "ord-1".to_owned(),
                    account_id: Some("acct-1".to_owned()),
                    symbol: "GCM2026".to_owned(),
                    side: Some(TradeSide::Buy),
                    quantity: Some(1),
                    order_type: Some(EntryOrderType::Limit),
                    status: BrokerOrderStatus::Working,
                    filled_quantity: 0,
                    average_fill_price: None,
                    updated_at: Utc::now(),
                })
                .into_iter()
                .collect(),
            fills: include_fill
                .then_some(BrokerFillUpdate {
                    fill_id: "fill-1".to_owned(),
                    broker_order_id: Some("ord-1".to_owned()),
                    account_id: Some("acct-1".to_owned()),
                    symbol: "GCM2026".to_owned(),
                    side: TradeSide::Buy,
                    quantity: 1,
                    price: Decimal::new(238_500, 2),
                    fee: Some(Decimal::new(125, 2)),
                    commission: Some(Decimal::new(75, 2)),
                    occurred_at: Utc::now(),
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn recorder_tracks_loaded_strategy_and_run_status() {
        let recorder = test_recorder();
        let started_at = Utc::now();

        recorder
            .record_strategy_loaded(
                "gc_history_v1".to_owned(),
                RuntimeMode::Paper,
                ActionSource::Cli,
                started_at,
            )
            .expect("strategy load should persist");
        recorder
            .record_mode_change(RuntimeMode::Live)
            .expect("mode change should persist");
        recorder
            .record_run_status(StrategyRunStatus::Active, started_at)
            .expect("run status should persist");

        let snapshot = recorder.snapshot().expect("history snapshot should load");
        assert_eq!(snapshot.projection.total_strategy_run_records, 3);
        assert_eq!(
            snapshot
                .projection
                .latest_run
                .as_ref()
                .map(|run| run.strategy_id.as_str()),
            Some("gc_history_v1")
        );
        assert_eq!(
            snapshot
                .projection
                .latest_run
                .as_ref()
                .map(|run| run.mode.clone()),
            Some(RuntimeMode::Live)
        );
        assert_eq!(
            snapshot
                .projection
                .latest_run
                .as_ref()
                .map(|run| run.status.clone()),
            Some(StrategyRunStatus::Active)
        );
    }

    #[test]
    fn recorder_syncs_broker_snapshots_into_trade_history() {
        let recorder = test_recorder();
        let occurred_at = Utc::now();

        recorder
            .record_strategy_loaded(
                "gc_history_v1".to_owned(),
                RuntimeMode::Paper,
                ActionSource::Cli,
                occurred_at,
            )
            .expect("strategy load should persist");
        recorder
            .sync_broker_snapshot(&sample_broker_snapshot(1, true, true), occurred_at)
            .expect("broker snapshot should persist");
        recorder
            .sync_broker_snapshot(&sample_broker_snapshot(0, false, false), occurred_at)
            .expect("flattened broker snapshot should persist");

        let snapshot = recorder.snapshot().expect("history snapshot should load");
        assert_eq!(snapshot.projection.total_order_records, 2);
        assert_eq!(snapshot.projection.total_fill_records, 1);
        assert_eq!(snapshot.projection.total_position_records, 2);
        assert_eq!(snapshot.projection.closed_trade_count, 1);
        assert!(snapshot.projection.latest_pnl_snapshot.is_some());
        assert!(snapshot.projection.open_trade_ids.is_empty());
        assert!(snapshot.projection.open_position_symbols.is_empty());
    }
}
