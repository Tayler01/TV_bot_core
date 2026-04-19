use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use rust_decimal::Decimal;
use thiserror::Error;
use tv_bot_control_api::{
    ControlApiCommand, HttpCommandRequest, HttpStatusCode, LoadedStrategySummary,
    ManualCommandSource, RuntimeAuthenticatedOperatorSnapshot, RuntimeAuthorizationSnapshot,
    RuntimeJournalStatus, RuntimeLifecycleCommand, RuntimeReadinessSnapshot,
    RuntimeReconnectReviewStatus, RuntimeShutdownReviewStatus, RuntimeStatusSnapshot,
    RuntimeStorageStatus,
};
#[cfg(test)]
use tv_bot_core_types::WarmupStatus;
use tv_bot_core_types::{
    BrokerOrderStatus, BrokerOrderUpdate, BrokerPositionSnapshot, BrokerPreference,
    BrokerStatusSnapshot, ExecutionIntent, InstrumentMapping, ReadinessCheck, ReadinessCheckStatus,
    RuntimeMode, SystemHealthSnapshot, TradePathLatencyRecord, TradeSide,
};
use tv_bot_execution_engine::{
    ExecutionInstrumentContext, ExecutionRequest, ExecutionStateContext,
};
use tv_bot_instrument_resolver::{FrontMonthResolver, StaticContractChainProvider};
use tv_bot_market_data::{MarketDataServiceSnapshot, WarmupProgress};
use tv_bot_risk_engine::{BrokerProtectionSupport, RiskInstrumentContext, RiskStateContext};
use tv_bot_runtime_kernel::{
    DependencyHealth, ReadinessEvaluator, ReadinessInputs, RuntimeExecutionRequest,
    RuntimeKernelError, RuntimeStateMachine,
};
use tv_bot_strategy_loader::{
    StrategyCompilation, StrategyCompileError, StrategyIssue, StrictStrategyCompiler,
};

#[derive(Clone, Debug)]
struct LoadedStrategyState {
    path: PathBuf,
    title: Option<String>,
    compiled: tv_bot_core_types::CompiledStrategy,
    warnings: Vec<StrategyIssue>,
    instrument_mapping: Option<InstrumentMapping>,
    instrument_resolution_error: Option<String>,
}

impl LoadedStrategyState {
    fn summary(&self) -> LoadedStrategySummary {
        LoadedStrategySummary {
            path: self.path.clone(),
            title: self.title.clone(),
            strategy_id: self.compiled.metadata.strategy_id.clone(),
            name: self.compiled.metadata.name.clone(),
            version: self.compiled.metadata.version.clone(),
            market_family: self.compiled.market.market.clone(),
            warning_count: self.warnings.len(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeStatusContext {
    pub http_bind: String,
    pub websocket_bind: String,
    pub command_dispatch_ready: bool,
    pub command_dispatch_detail: String,
    pub broker_status: Option<BrokerStatusSnapshot>,
    pub market_data_status: Option<MarketDataServiceSnapshot>,
    pub market_data_detail: Option<String>,
    pub storage_status: RuntimeStorageStatus,
    pub journal_status: RuntimeJournalStatus,
    pub system_health: Option<SystemHealthSnapshot>,
    pub latest_trade_latency: Option<TradePathLatencyRecord>,
    pub recorded_trade_latency_count: usize,
    pub open_positions: Vec<BrokerPositionSnapshot>,
    pub working_orders: Vec<BrokerOrderUpdate>,
    pub authenticated_operator: Option<RuntimeAuthenticatedOperatorSnapshot>,
    pub authorization: RuntimeAuthorizationSnapshot,
    pub reconnect_review: RuntimeReconnectReviewStatus,
    pub shutdown_review: RuntimeShutdownReviewStatus,
}

#[derive(Clone, Debug)]
pub struct LoadedStrategyMarketDataSeed {
    pub strategy: tv_bot_core_types::CompiledStrategy,
    pub instrument_mapping: Option<InstrumentMapping>,
    pub instrument_resolution_error: Option<String>,
}

#[derive(Debug)]
pub struct RuntimeOperatorState {
    runtime: RuntimeStateMachine,
    loaded_strategy: Option<LoadedStrategyState>,
}

#[derive(Debug, Error)]
pub enum RuntimeOperatorError {
    #[error("failed to read strategy file `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Compile(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("runtime transition failed: {source}")]
    RuntimeKernel {
        #[source]
        source: RuntimeKernelError,
    },
    #[error("no strategy is currently loaded")]
    StrategyNotLoaded,
}

impl RuntimeOperatorError {
    pub fn status_code(&self) -> HttpStatusCode {
        match self {
            Self::RuntimeKernel {
                source: RuntimeKernelError::OverrideRequired,
            } => HttpStatusCode::PreconditionRequired,
            Self::Io { .. } => HttpStatusCode::InternalServerError,
            Self::Compile(_)
            | Self::InvalidRequest(_)
            | Self::RuntimeKernel { .. }
            | Self::StrategyNotLoaded => HttpStatusCode::Conflict,
        }
    }
}

impl RuntimeOperatorState {
    pub fn new(runtime: RuntimeStateMachine) -> Self {
        Self {
            runtime,
            loaded_strategy: None,
        }
    }

    pub fn status_snapshot(&self, context: &RuntimeStatusContext) -> RuntimeStatusSnapshot {
        RuntimeStatusSnapshot {
            mode: self.runtime.current_mode(),
            arm_state: self.runtime.arm_state(),
            warmup_status: self.runtime.warmup_status(),
            strategy_loaded: self.runtime.is_strategy_loaded(),
            hard_override_active: self.runtime.hard_override_active(),
            operator_new_entries_enabled: self.runtime.new_entries_enabled(),
            operator_new_entries_reason: self.runtime.new_entries_reason(),
            current_strategy: self
                .loaded_strategy
                .as_ref()
                .map(LoadedStrategyState::summary),
            broker_status: context.broker_status.clone(),
            market_data_status: context.market_data_status.clone(),
            market_data_detail: context.market_data_detail.clone(),
            storage_status: context.storage_status.clone(),
            journal_status: context.journal_status.clone(),
            system_health: context.system_health.clone(),
            latest_trade_latency: context.latest_trade_latency.clone(),
            recorded_trade_latency_count: context.recorded_trade_latency_count,
            current_account_name: context
                .broker_status
                .as_ref()
                .and_then(|snapshot| snapshot.selected_account.as_ref())
                .map(|selection| selection.account_name.clone()),
            authenticated_operator: context.authenticated_operator.clone(),
            authorization: context.authorization.clone(),
            instrument_mapping: self
                .loaded_strategy
                .as_ref()
                .and_then(|strategy| strategy.instrument_mapping.clone()),
            instrument_resolution_error: self
                .loaded_strategy
                .as_ref()
                .and_then(|strategy| strategy.instrument_resolution_error.clone()),
            reconnect_review: context.reconnect_review.clone(),
            shutdown_review: context.shutdown_review.clone(),
            http_bind: context.http_bind.clone(),
            websocket_bind: context.websocket_bind.clone(),
            command_dispatch_ready: context.command_dispatch_ready,
            command_dispatch_detail: context.command_dispatch_detail.clone(),
        }
    }

    pub fn readiness_snapshot(&self, context: &RuntimeStatusContext) -> RuntimeReadinessSnapshot {
        let status = self.status_snapshot(context);
        let mut symbol_mapping_override = None;
        let mut hard_override_reasons = Vec::new();

        if let Some(loaded_strategy) = &self.loaded_strategy {
            if let Some(error) = &loaded_strategy.instrument_resolution_error {
                let message = format!("instrument resolution requires review: {error}");
                symbol_mapping_override = Some(message.clone());
                hard_override_reasons.push(message);
            }
        }

        let account_selection = self.account_selection_health(context);
        let market_data = self.market_data_health(context);
        let broker_sync = self.broker_sync_health(context);
        let storage = self.storage_health(context);
        let journal = self.journal_health(context);

        if let DependencyHealth::Warning(message) = &storage {
            hard_override_reasons.push(message.clone());
        }
        if let DependencyHealth::Warning(message) = &journal {
            hard_override_reasons.push(message.clone());
        }

        let mut report = ReadinessEvaluator::evaluate(ReadinessInputs {
            mode: self.runtime.current_mode(),
            strategy_loaded: self.runtime.is_strategy_loaded(),
            warmup_status: self.runtime.warmup_status(),
            account_selection,
            symbol_mapping_resolved: self.loaded_strategy.is_none()
                || self
                    .loaded_strategy
                    .as_ref()
                    .and_then(|strategy| strategy.instrument_mapping.as_ref())
                    .is_some(),
            market_data,
            broker_sync,
            storage,
            journal,
            clock: DependencyHealth::Healthy,
            risk_summary: self.risk_summary(),
            hard_override_reasons,
        });

        if let Some(message) = symbol_mapping_override {
            if let Some(check) = report
                .checks
                .iter_mut()
                .find(|check| check.name == "symbol_mapping")
            {
                check.status = ReadinessCheckStatus::Warning;
                check.message = message;
            }
        }

        if !self.runtime.new_entries_enabled() {
            let message = self
                .runtime
                .new_entries_reason()
                .map(|reason| format!("new entries are disabled by operator control: {reason}"))
                .unwrap_or_else(|| "new entries are disabled by operator control".to_owned());
            report.checks.push(ReadinessCheck {
                name: "operator_entry_gate".to_owned(),
                status: ReadinessCheckStatus::Warning,
                message,
            });
        }

        RuntimeReadinessSnapshot { status, report }
    }

    pub fn apply_lifecycle_command(
        &mut self,
        command: RuntimeLifecycleCommand,
        context: &RuntimeStatusContext,
    ) -> Result<String, RuntimeOperatorError> {
        match command {
            RuntimeLifecycleCommand::SetMode { mode } => {
                self.runtime.switch_mode(mode.clone());
                Ok(format!("runtime mode set to {}", mode_label(&mode)))
            }
            RuntimeLifecycleCommand::LoadStrategy { path } => {
                self.load_strategy(&path)?;
                let summary = self
                    .loaded_strategy
                    .as_ref()
                    .expect("strategy should be loaded")
                    .summary();
                Ok(format!(
                    "loaded strategy `{}` from `{}`",
                    summary.strategy_id,
                    summary.path.display()
                ))
            }
            RuntimeLifecycleCommand::StartWarmup => {
                self.runtime
                    .start_warmup()
                    .map_err(|source| RuntimeOperatorError::RuntimeKernel { source })?;
                Ok("warmup started".to_owned())
            }
            RuntimeLifecycleCommand::MarkWarmupReady => {
                self.runtime
                    .mark_warmup_ready()
                    .map_err(|source| RuntimeOperatorError::RuntimeKernel { source })?;
                Ok("warmup marked ready".to_owned())
            }
            RuntimeLifecycleCommand::MarkWarmupFailed { reason } => {
                self.runtime
                    .mark_warmup_failed()
                    .map_err(|source| RuntimeOperatorError::RuntimeKernel { source })?;
                Ok(match reason {
                    Some(reason) if !reason.trim().is_empty() => {
                        format!("warmup marked failed: {reason}")
                    }
                    _ => "warmup marked failed".to_owned(),
                })
            }
            RuntimeLifecycleCommand::Arm { allow_override } => {
                let readiness = self.readiness_snapshot(context);
                self.runtime
                    .arm(&readiness.report, allow_override)
                    .map_err(|source| RuntimeOperatorError::RuntimeKernel { source })?;
                Ok(if allow_override {
                    "runtime armed with temporary override".to_owned()
                } else {
                    "runtime armed".to_owned()
                })
            }
            RuntimeLifecycleCommand::Disarm => {
                self.runtime.disarm();
                Ok("runtime disarmed".to_owned())
            }
            RuntimeLifecycleCommand::Pause => {
                self.runtime.pause();
                Ok("runtime paused".to_owned())
            }
            RuntimeLifecycleCommand::Resume => {
                self.runtime
                    .resume()
                    .map_err(|source| RuntimeOperatorError::RuntimeKernel { source })?;
                Ok(format!(
                    "runtime resumed into {}",
                    mode_label(&self.runtime.current_mode())
                ))
            }
            RuntimeLifecycleCommand::SetNewEntriesEnabled { enabled, reason } => {
                self.runtime
                    .set_new_entries_enabled(enabled, reason.clone());
                Ok(if enabled {
                    "new entries enabled".to_owned()
                } else {
                    match reason {
                        Some(reason) if !reason.trim().is_empty() => {
                            format!("new entries disabled: {reason}")
                        }
                        _ => "new entries disabled".to_owned(),
                    }
                })
            }
            RuntimeLifecycleCommand::ResolveReconnectReview { .. } => {
                Ok("reconnect review prepared".to_owned())
            }
            RuntimeLifecycleCommand::Shutdown { .. } => Ok("shutdown review prepared".to_owned()),
            RuntimeLifecycleCommand::ClosePosition { .. } => {
                Ok("close position prepared".to_owned())
            }
            RuntimeLifecycleCommand::ManualEntry { .. } => Ok("manual entry prepared".to_owned()),
            RuntimeLifecycleCommand::CancelWorkingOrders { .. } => {
                Ok("working-order cancellation prepared".to_owned())
            }
            RuntimeLifecycleCommand::Flatten { .. } => Ok("flatten prepared".to_owned()),
        }
    }

    pub fn build_close_position_request(
        &self,
        context: &RuntimeStatusContext,
        source: ManualCommandSource,
        contract_id: Option<i64>,
        reason: Option<String>,
    ) -> Result<HttpCommandRequest, RuntimeOperatorError> {
        let resolved_contract_id = self.resolve_active_contract_id(context, contract_id)?;
        self.build_flatten_request(
            context,
            source,
            resolved_contract_id,
            reason.unwrap_or_else(|| "manual close position".to_owned()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_manual_entry_request(
        &self,
        context: &RuntimeStatusContext,
        source: ManualCommandSource,
        side: TradeSide,
        quantity: u32,
        tick_size: Decimal,
        entry_reference_price: Decimal,
        tick_value_usd: Option<Decimal>,
        reason: Option<String>,
    ) -> Result<HttpCommandRequest, RuntimeOperatorError> {
        if quantity == 0 {
            return Err(RuntimeOperatorError::InvalidRequest(
                "manual entry quantity must be greater than zero".to_owned(),
            ));
        }

        if tick_size <= Decimal::ZERO {
            return Err(RuntimeOperatorError::InvalidRequest(
                "manual entry tick size must be greater than zero".to_owned(),
            ));
        }

        if entry_reference_price <= Decimal::ZERO {
            return Err(RuntimeOperatorError::InvalidRequest(
                "manual entry reference price must be greater than zero".to_owned(),
            ));
        }

        if tick_value_usd.is_some_and(|value| value <= Decimal::ZERO) {
            return Err(RuntimeOperatorError::InvalidRequest(
                "manual entry tick value must be greater than zero when provided".to_owned(),
            ));
        }

        let strategy = self.loaded_strategy()?;
        let tradovate_symbol = self.loaded_market_symbol(context)?;
        let current_position = self.active_position_for_symbol(context, &tradovate_symbol, None);
        let active_contract_id = current_position.as_ref().and_then(position_contract_id);

        Ok(HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source,
                request: RuntimeExecutionRequest {
                    mode: self.runtime.current_mode(),
                    action_source: source.into(),
                    authenticated_operator: None,
                    execution: ExecutionRequest {
                        strategy: strategy.compiled.clone(),
                        instrument: ExecutionInstrumentContext {
                            tradovate_symbol: tradovate_symbol.clone(),
                            tick_size,
                            entry_reference_price: Some(entry_reference_price),
                            active_contract_id,
                        },
                        state: ExecutionStateContext {
                            // Sanitization re-applies runtime submission and broker snapshot
                            // guards before dispatch, but the entry builder starts from the
                            // loaded market context so the request stays strategy-agnostic.
                            runtime_can_submit_orders: true,
                            new_entries_allowed: true,
                            current_position: current_position.clone(),
                            working_orders: self
                                .working_orders_for_symbol(context, &tradovate_symbol),
                        },
                        intent: ExecutionIntent::Enter {
                            side,
                            order_type: strategy.compiled.entry_rules.entry_order_type,
                            quantity,
                            protective_brackets_expected: self
                                .manual_entry_uses_broker_side_protection(&strategy.compiled),
                            reason: reason.unwrap_or_else(|| "dashboard manual entry".to_owned()),
                        },
                    },
                    risk_instrument: RiskInstrumentContext { tick_value_usd },
                    risk_state: RiskStateContext {
                        current_position: current_position.clone(),
                        unrealized_pnl: current_position
                            .as_ref()
                            .and_then(|position| position.unrealized_pnl),
                        hard_override_active: self.runtime.hard_override_active(),
                        ..RiskStateContext {
                            trades_today: 0,
                            consecutive_losses: 0,
                            current_position: None,
                            unrealized_pnl: None,
                            broker_support: BrokerProtectionSupport::default(),
                            hard_override_active: false,
                        }
                    },
                },
            },
        })
    }

    pub fn build_cancel_working_orders_request(
        &self,
        context: &RuntimeStatusContext,
        source: ManualCommandSource,
        reason: Option<String>,
    ) -> Result<HttpCommandRequest, RuntimeOperatorError> {
        let strategy = self.loaded_strategy()?;
        let tradovate_symbol = self.loaded_market_symbol(context)?;
        let current_position = self.active_position_for_symbol(context, &tradovate_symbol, None);
        let working_orders = self.working_orders_for_symbol(context, &tradovate_symbol);

        if working_orders.is_empty() {
            return Err(RuntimeOperatorError::InvalidRequest(
                "no working orders are currently active for the loaded market".to_owned(),
            ));
        }

        Ok(HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source,
                request: RuntimeExecutionRequest {
                    mode: self.runtime.current_mode(),
                    action_source: source.into(),
                    authenticated_operator: None,
                    execution: ExecutionRequest {
                        strategy: strategy.compiled.clone(),
                        instrument: ExecutionInstrumentContext {
                            tradovate_symbol: tradovate_symbol.clone(),
                            // Working-order cancellation routes by broker order id, so keep a
                            // non-zero tick size without introducing a fake market-data lookup.
                            tick_size: Decimal::ONE,
                            entry_reference_price: None,
                            active_contract_id: None,
                        },
                        state: ExecutionStateContext {
                            // Safety cancellations must remain available even when the runtime is
                            // paused or disarmed.
                            runtime_can_submit_orders: true,
                            new_entries_allowed: false,
                            current_position: current_position.clone(),
                            working_orders,
                        },
                        intent: ExecutionIntent::CancelWorkingOrders {
                            reason: reason
                                .unwrap_or_else(|| "manual cancel working orders".to_owned()),
                        },
                    },
                    risk_instrument: RiskInstrumentContext::default(),
                    risk_state: RiskStateContext {
                        current_position: current_position.clone(),
                        unrealized_pnl: current_position
                            .as_ref()
                            .and_then(|position| position.unrealized_pnl),
                        hard_override_active: self.runtime.hard_override_active(),
                        ..RiskStateContext {
                            trades_today: 0,
                            consecutive_losses: 0,
                            current_position: None,
                            unrealized_pnl: None,
                            broker_support: BrokerProtectionSupport::default(),
                            hard_override_active: false,
                        }
                    },
                },
            },
        })
    }

    pub fn build_flatten_request(
        &self,
        context: &RuntimeStatusContext,
        source: ManualCommandSource,
        contract_id: i64,
        reason: String,
    ) -> Result<HttpCommandRequest, RuntimeOperatorError> {
        let strategy = self.loaded_strategy()?;
        let tradovate_symbol = self.loaded_market_symbol(context)?;
        let current_position =
            self.active_position_for_symbol(context, &tradovate_symbol, Some(contract_id));

        Ok(HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source,
                request: RuntimeExecutionRequest {
                    mode: self.runtime.current_mode(),
                    action_source: source.into(),
                    authenticated_operator: None,
                    execution: ExecutionRequest {
                        strategy: strategy.compiled.clone(),
                        instrument: ExecutionInstrumentContext {
                            tradovate_symbol: tradovate_symbol.clone(),
                            // Flatten uses the contract id, not tick math. A non-zero value keeps
                            // the shared execution validation explicit without adding a fake
                            // broker-specific tick lookup here.
                            tick_size: Decimal::ONE,
                            entry_reference_price: None,
                            active_contract_id: Some(contract_id),
                        },
                        state: ExecutionStateContext {
                            // Safety exits must remain available even when the runtime is paused
                            // or disarmed, because flatten is part of recovery and shutdown flow.
                            runtime_can_submit_orders: true,
                            new_entries_allowed: false,
                            current_position: current_position.clone(),
                            working_orders: self
                                .working_orders_for_symbol(context, &tradovate_symbol),
                        },
                        intent: ExecutionIntent::Flatten { reason },
                    },
                    risk_instrument: RiskInstrumentContext::default(),
                    risk_state: RiskStateContext {
                        current_position: current_position.clone(),
                        unrealized_pnl: current_position
                            .as_ref()
                            .and_then(|position| position.unrealized_pnl),
                        hard_override_active: self.runtime.hard_override_active(),
                        ..RiskStateContext {
                            trades_today: 0,
                            consecutive_losses: 0,
                            current_position: None,
                            unrealized_pnl: None,
                            broker_support: BrokerProtectionSupport::default(),
                            hard_override_active: false,
                        }
                    },
                },
            },
        })
    }

    pub fn resolve_active_contract_id(
        &self,
        context: &RuntimeStatusContext,
        requested_contract_id: Option<i64>,
    ) -> Result<i64, RuntimeOperatorError> {
        if let Some(contract_id) = requested_contract_id {
            if contract_id > 0 {
                return Ok(contract_id);
            }

            return Err(RuntimeOperatorError::InvalidRequest(
                "contract id must be greater than zero".to_owned(),
            ));
        }

        let contract_ids = context
            .open_positions
            .iter()
            .filter(|position| position.quantity != 0)
            .filter_map(position_contract_id)
            .collect::<BTreeSet<_>>();

        match contract_ids.len() {
            0 => Err(RuntimeOperatorError::InvalidRequest(
                "no open position with a resolvable Tradovate contract id is currently available"
                    .to_owned(),
            )),
            1 => contract_ids.iter().next().copied().ok_or_else(|| {
                RuntimeOperatorError::InvalidRequest(
                    "no open position with a resolvable Tradovate contract id is currently available"
                        .to_owned(),
                )
            }),
            _ => Err(RuntimeOperatorError::InvalidRequest(
                "multiple open positions are active; an explicit contract id is required"
                    .to_owned(),
            )),
        }
    }

    pub fn all_open_positions_broker_protected(&self, context: &RuntimeStatusContext) -> bool {
        let open_positions = context
            .open_positions
            .iter()
            .filter(|position| position.quantity != 0)
            .collect::<Vec<_>>();

        !open_positions.is_empty()
            && open_positions
                .iter()
                .all(|position| position.protective_orders_present)
    }

    pub fn sanitize_command_request(
        &self,
        context: &RuntimeStatusContext,
        mut request: HttpCommandRequest,
    ) -> Result<HttpCommandRequest, RuntimeOperatorError> {
        let strategy = self.loaded_strategy()?;
        let runtime_mode = self.runtime.current_mode();
        let runtime_can_submit_orders = self.runtime.can_submit_orders();
        let hard_override_active = self.runtime.hard_override_active();
        let runtime_allows_new_entries = self.new_entries_allowed(context);
        let mapped_symbol = strategy
            .instrument_mapping
            .as_ref()
            .map(|mapping| mapping.tradovate_symbol.clone());

        match &mut request.command {
            ControlApiCommand::ManualIntent { request, .. }
            | ControlApiCommand::StrategyIntent { request } => {
                request.mode = runtime_mode;
                request.execution.strategy = strategy.compiled.clone();
                request.execution.state.runtime_can_submit_orders = runtime_can_submit_orders;
                request.execution.state.new_entries_allowed =
                    request.execution.state.new_entries_allowed && runtime_allows_new_entries;
                request.risk_state.hard_override_active = hard_override_active;

                if let Some(symbol) = mapped_symbol {
                    request.execution.instrument.tradovate_symbol = symbol;
                }

                let effective_symbol = request.execution.instrument.tradovate_symbol.clone();
                let current_position = self.active_position_for_symbol(
                    context,
                    &effective_symbol,
                    request.execution.instrument.active_contract_id,
                );

                request.execution.state.current_position = current_position.clone();
                request.execution.state.working_orders =
                    self.working_orders_for_symbol(context, &effective_symbol);
                request.risk_state.current_position = current_position.clone();
                request.risk_state.unrealized_pnl = current_position
                    .as_ref()
                    .and_then(|position| position.unrealized_pnl);
                request.risk_state.broker_support = self.broker_protection_support(context);
            }
        }

        Ok(request)
    }

    pub fn market_data_seed(&self) -> Result<LoadedStrategyMarketDataSeed, RuntimeOperatorError> {
        let strategy = self.loaded_strategy()?;

        Ok(LoadedStrategyMarketDataSeed {
            strategy: strategy.compiled.clone(),
            instrument_mapping: strategy.instrument_mapping.clone(),
            instrument_resolution_error: strategy.instrument_resolution_error.clone(),
        })
    }

    pub fn sync_market_data_warmup(
        &mut self,
        progress: &WarmupProgress,
    ) -> Result<(), RuntimeOperatorError> {
        self.runtime
            .sync_warmup_progress(progress)
            .map_err(|source| RuntimeOperatorError::RuntimeKernel { source })
    }

    fn load_strategy(&mut self, path: &Path) -> Result<(), RuntimeOperatorError> {
        let markdown = fs::read_to_string(path).map_err(|source| RuntimeOperatorError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let compilation = StrictStrategyCompiler
            .compile_markdown(&markdown)
            .map_err(|source| RuntimeOperatorError::Compile(format_compile_error(&source)))?;
        let loaded_strategy = compile_loaded_strategy(path.to_path_buf(), compilation);

        self.runtime.mark_strategy_loaded();
        self.loaded_strategy = Some(loaded_strategy);
        Ok(())
    }

    fn loaded_strategy(&self) -> Result<&LoadedStrategyState, RuntimeOperatorError> {
        self.loaded_strategy
            .as_ref()
            .ok_or(RuntimeOperatorError::StrategyNotLoaded)
    }

    fn loaded_market_symbol(
        &self,
        context: &RuntimeStatusContext,
    ) -> Result<String, RuntimeOperatorError> {
        let strategy = self.loaded_strategy()?;
        if let Some(mapping) = strategy.instrument_mapping.as_ref() {
            return Ok(mapping.tradovate_symbol.clone());
        }

        let mut working_symbols = context
            .working_orders
            .iter()
            .filter(|order| order.status == BrokerOrderStatus::Working)
            .map(|order| order.symbol.clone())
            .collect::<BTreeSet<_>>();
        if working_symbols.len() == 1 {
            return working_symbols.pop_first().ok_or_else(|| {
                RuntimeOperatorError::InvalidRequest(
                    "working-order symbol inference unexpectedly failed".to_owned(),
                )
            });
        }

        let mut position_symbols = context
            .open_positions
            .iter()
            .filter(|position| position.quantity != 0)
            .map(|position| position.symbol.clone())
            .filter(|symbol| !symbol.starts_with("contract:"))
            .collect::<BTreeSet<_>>();
        if position_symbols.len() == 1 {
            return position_symbols.pop_first().ok_or_else(|| {
                RuntimeOperatorError::InvalidRequest(
                    "position symbol inference unexpectedly failed".to_owned(),
                )
            });
        }

        Ok(strategy.compiled.market.market.clone())
    }

    fn broker_protection_support(&self, context: &RuntimeStatusContext) -> BrokerProtectionSupport {
        // The local operator only knows how to drive Tradovate in V1. Until broker
        // status exposes structured capability flags, infer the supported broker-side
        // protections from the active provider so risk decisions can stay aligned with
        // the real execution path.
        let tradovate_active = context
            .broker_status
            .as_ref()
            .is_some_and(|status| status.provider.eq_ignore_ascii_case("tradovate"));

        if tradovate_active {
            BrokerProtectionSupport {
                stop_loss: true,
                take_profit: true,
                trailing_stop: false,
                daily_loss_limit: false,
            }
        } else {
            BrokerProtectionSupport::default()
        }
    }

    fn manual_entry_uses_broker_side_protection(
        &self,
        strategy: &tv_bot_core_types::CompiledStrategy,
    ) -> bool {
        let stop_configured = strategy.trade_management.initial_stop_ticks > 0;
        let take_profit_configured = strategy.trade_management.take_profit_ticks > 0;

        let broker_prefers_stop =
            strategy.execution.broker_preferences.stop_loss != BrokerPreference::BotAllowed;
        let broker_prefers_take_profit =
            strategy.execution.broker_preferences.take_profit != BrokerPreference::BotAllowed;

        (stop_configured && broker_prefers_stop)
            || (take_profit_configured && broker_prefers_take_profit)
    }

    fn risk_summary(&self) -> String {
        self.loaded_strategy
            .as_ref()
            .map(|strategy| {
                let sizing = match strategy.compiled.position_sizing.mode {
                    tv_bot_core_types::PositionSizingMode::Fixed => format!(
                        "fixed sizing: {} contract(s)",
                        strategy.compiled.position_sizing.contracts.unwrap_or(0)
                    ),
                    tv_bot_core_types::PositionSizingMode::RiskBased => format!(
                        "risk-based sizing: max {} USD",
                        strategy
                            .compiled
                            .position_sizing
                            .max_risk_usd
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unknown".to_owned())
                    ),
                };

                format!(
                    "{sizing}; broker prefs stop={:?} tp={:?} trail={:?}",
                    strategy.compiled.execution.broker_preferences.stop_loss,
                    strategy.compiled.execution.broker_preferences.take_profit,
                    strategy.compiled.execution.broker_preferences.trailing_stop,
                )
            })
            .unwrap_or_default()
    }

    fn account_selection_health(&self, context: &RuntimeStatusContext) -> DependencyHealth {
        if self.runtime.is_strategy_loaded()
            && matches!(
                self.runtime.current_mode(),
                RuntimeMode::Paper | RuntimeMode::Live
            )
        {
            if let Some(snapshot) = context.broker_status.as_ref() {
                ReadinessEvaluator::broker_account_dependency(
                    &self.runtime.current_mode(),
                    snapshot,
                )
            } else if context.command_dispatch_ready {
                DependencyHealth::Blocking(
                    "broker session snapshot is not yet available through the runtime host"
                        .to_owned(),
                )
            } else {
                DependencyHealth::Blocking(context.command_dispatch_detail.clone())
            }
        } else {
            DependencyHealth::Healthy
        }
    }

    fn market_data_health(&self, context: &RuntimeStatusContext) -> DependencyHealth {
        if self.runtime.is_strategy_loaded()
            && matches!(
                self.runtime.current_mode(),
                RuntimeMode::Paper | RuntimeMode::Live
            )
        {
            if let Some(snapshot) = context.market_data_status.as_ref() {
                ReadinessEvaluator::market_data_dependency(&snapshot.session.market_data)
            } else if let Some(detail) = context.market_data_detail.as_ref() {
                DependencyHealth::Blocking(detail.clone())
            } else {
                DependencyHealth::Blocking(
                    "market-data service is not yet available through the runtime host".to_owned(),
                )
            }
        } else {
            DependencyHealth::Healthy
        }
    }

    fn broker_sync_health(&self, context: &RuntimeStatusContext) -> DependencyHealth {
        if self.runtime.is_strategy_loaded()
            && matches!(
                self.runtime.current_mode(),
                RuntimeMode::Paper | RuntimeMode::Live
            )
        {
            if let Some(snapshot) = context.broker_status.as_ref() {
                ReadinessEvaluator::broker_sync_dependency(snapshot)
            } else if context.command_dispatch_ready {
                DependencyHealth::Blocking(
                    "broker sync snapshot is not yet available through the runtime host".to_owned(),
                )
            } else {
                DependencyHealth::Blocking(context.command_dispatch_detail.clone())
            }
        } else {
            DependencyHealth::Healthy
        }
    }

    fn storage_health(&self, context: &RuntimeStatusContext) -> DependencyHealth {
        if self.runtime.is_strategy_loaded()
            && matches!(
                self.runtime.current_mode(),
                RuntimeMode::Paper | RuntimeMode::Live
            )
        {
            if !context.storage_status.durable {
                DependencyHealth::Blocking(context.storage_status.detail.clone())
            } else if context.storage_status.fallback_activated {
                if context.storage_status.allow_runtime_fallback {
                    DependencyHealth::Warning(context.storage_status.detail.clone())
                } else {
                    DependencyHealth::Blocking(context.storage_status.detail.clone())
                }
            } else {
                match context.storage_status.mode {
                    tv_bot_control_api::RuntimeStorageMode::Unconfigured => {
                        DependencyHealth::Blocking(context.storage_status.detail.clone())
                    }
                    tv_bot_control_api::RuntimeStorageMode::PrimaryConfigured => {
                        DependencyHealth::Healthy
                    }
                    tv_bot_control_api::RuntimeStorageMode::SqliteFallbackOnly => {
                        if context.storage_status.allow_runtime_fallback {
                            DependencyHealth::Warning(context.storage_status.detail.clone())
                        } else {
                            DependencyHealth::Blocking(context.storage_status.detail.clone())
                        }
                    }
                }
            }
        } else {
            DependencyHealth::Healthy
        }
    }

    fn journal_health(&self, context: &RuntimeStatusContext) -> DependencyHealth {
        if self.runtime.is_strategy_loaded()
            && matches!(
                self.runtime.current_mode(),
                RuntimeMode::Paper | RuntimeMode::Live
            )
        {
            if context.journal_status.durable {
                DependencyHealth::Healthy
            } else {
                DependencyHealth::Warning(context.journal_status.detail.clone())
            }
        } else {
            DependencyHealth::Healthy
        }
    }

    fn active_position_for_symbol(
        &self,
        context: &RuntimeStatusContext,
        tradovate_symbol: &str,
        contract_id: Option<i64>,
    ) -> Option<BrokerPositionSnapshot> {
        let contract_symbol = contract_id.map(|value| format!("contract:{value}"));

        context
            .open_positions
            .iter()
            .find(|position| {
                position.quantity != 0
                    && contract_symbol
                        .as_deref()
                        .map(|expected| position.symbol == expected)
                        .unwrap_or(false)
            })
            .or_else(|| {
                context.open_positions.iter().find(|position| {
                    position.quantity != 0 && position.symbol.eq_ignore_ascii_case(tradovate_symbol)
                })
            })
            .or_else(|| {
                context
                    .open_positions
                    .iter()
                    .find(|position| position.quantity != 0)
            })
            .cloned()
    }

    fn working_orders_for_symbol(
        &self,
        context: &RuntimeStatusContext,
        tradovate_symbol: &str,
    ) -> Vec<BrokerOrderUpdate> {
        context
            .working_orders
            .iter()
            .filter(|order| {
                order.status == BrokerOrderStatus::Working
                    && order.symbol.eq_ignore_ascii_case(tradovate_symbol)
            })
            .cloned()
            .collect()
    }

    fn new_entries_allowed(&self, context: &RuntimeStatusContext) -> bool {
        self.runtime.new_entries_enabled()
            && !matches!(
                self.market_data_health(context),
                DependencyHealth::Blocking(_)
            )
            && !matches!(
                self.broker_sync_health(context),
                DependencyHealth::Blocking(_)
            )
    }
}

fn position_contract_id(position: &BrokerPositionSnapshot) -> Option<i64> {
    position
        .symbol
        .strip_prefix("contract:")
        .and_then(|contract_id| contract_id.parse::<i64>().ok())
}

fn compile_loaded_strategy(path: PathBuf, compilation: StrategyCompilation) -> LoadedStrategyState {
    let resolver =
        FrontMonthResolver::with_system_clock(StaticContractChainProvider::with_builtin_chains());
    let (instrument_mapping, instrument_resolution_error) =
        match resolver.resolve_for_strategy(&compilation.compiled) {
            Ok(mapping) => (Some(mapping), None),
            Err(error) => (None, Some(error.to_string())),
        };

    LoadedStrategyState {
        path,
        title: compilation.title,
        compiled: compilation.compiled,
        warnings: compilation.warnings,
        instrument_mapping,
        instrument_resolution_error,
    }
}

fn format_compile_error(error: &StrategyCompileError) -> String {
    let summary = error
        .errors
        .iter()
        .map(|issue| issue.message.clone())
        .collect::<Vec<_>>()
        .join("; ");

    format!("strategy compilation failed: {summary}")
}

fn mode_label(mode: &RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::Paper => "paper",
        RuntimeMode::Live => "live",
        RuntimeMode::Observation => "observation",
        RuntimeMode::Paused => "paused",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use async_trait::async_trait;
    use secrecy::SecretString;
    use tv_bot_broker_tradovate::{
        TradovateAccessToken, TradovateAccount, TradovateAccountApi, TradovateAccountListRequest,
        TradovateAuthApi, TradovateAuthRequest, TradovateCancelOrderRequest,
        TradovateCancelOrderResult, TradovateCredentials, TradovateError, TradovateExecutionApi,
        TradovateLiquidatePositionRequest, TradovateLiquidatePositionResult,
        TradovatePlaceOrderRequest, TradovatePlaceOrderResult, TradovatePlaceOsoRequest,
        TradovatePlaceOsoResult, TradovateRoutingPreferences, TradovateSessionConfig,
        TradovateSessionManager, TradovateSyncApi, TradovateSyncConnectRequest, TradovateSyncEvent,
        TradovateSyncSnapshot, TradovateUserSyncRequest,
    };
    use tv_bot_control_api::{ControlApiCommand, ManualCommandSource, RuntimeLifecycleCommand};
    use tv_bot_core_types::{ActionSource, EntryOrderType, RiskDecisionStatus};
    use tv_bot_journal::{EventJournal, InMemoryJournal};
    use tv_bot_runtime_kernel::{RuntimeCommand, RuntimeControlLoop};

    use super::*;

    fn sample_context() -> RuntimeStatusContext {
        RuntimeStatusContext {
            http_bind: "127.0.0.1:8080".to_owned(),
            websocket_bind: "127.0.0.1:8081".to_owned(),
            command_dispatch_ready: true,
            command_dispatch_detail: "tradovate dispatch configured".to_owned(),
            broker_status: Some(sample_broker_status()),
            market_data_status: Some(sample_market_data_status()),
            market_data_detail: None,
            storage_status: sample_storage_status(),
            journal_status: sample_journal_status(),
            system_health: Some(sample_system_health()),
            latest_trade_latency: Some(sample_trade_latency()),
            recorded_trade_latency_count: 1,
            open_positions: Vec::new(),
            working_orders: Vec::new(),
            authenticated_operator: None,
            authorization: RuntimeAuthorizationSnapshot {
                can_view: true,
                can_manage_runtime: true,
                can_manage_strategies: true,
                can_update_settings: true,
                can_trade: true,
                detail: "local runtime access allows full control".to_owned(),
            },
            reconnect_review: RuntimeReconnectReviewStatus {
                required: false,
                reason: None,
                last_decision: None,
                open_position_count: 0,
                working_order_count: 0,
            },
            shutdown_review: RuntimeShutdownReviewStatus {
                pending_signal: false,
                blocked: false,
                awaiting_flatten: false,
                decision: None,
                reason: None,
                open_position_count: 0,
                all_positions_broker_protected: false,
            },
        }
    }

    fn sample_broker_status() -> BrokerStatusSnapshot {
        BrokerStatusSnapshot {
            provider: "tradovate".to_owned(),
            environment: tv_bot_core_types::BrokerEnvironment::Demo,
            connection_state: tv_bot_core_types::BrokerConnectionState::Connected,
            health: tv_bot_core_types::BrokerHealth::Healthy,
            sync_state: tv_bot_core_types::BrokerSyncState::Synchronized,
            selected_account: Some(tv_bot_core_types::BrokerAccountSelection {
                provider: "tradovate".to_owned(),
                account_id: "101".to_owned(),
                account_name: "paper-primary".to_owned(),
                routing: tv_bot_core_types::BrokerAccountRouting::Paper,
                environment: tv_bot_core_types::BrokerEnvironment::Demo,
                selected_at: chrono::Utc::now(),
            }),
            reconnect_count: 0,
            last_authenticated_at: Some(chrono::Utc::now()),
            last_heartbeat_at: Some(chrono::Utc::now()),
            last_sync_at: Some(chrono::Utc::now()),
            last_disconnect_reason: None,
            review_required_reason: None,
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_position(symbol: &str, quantity: i32) -> BrokerPositionSnapshot {
        BrokerPositionSnapshot {
            account_id: Some("101".to_owned()),
            symbol: symbol.to_owned(),
            quantity,
            average_price: Some(Decimal::new(238_500, 2)),
            realized_pnl: None,
            unrealized_pnl: Some(Decimal::new(-75, 0)),
            protective_orders_present: true,
            captured_at: chrono::Utc::now(),
        }
    }

    fn sample_working_order(symbol: &str) -> BrokerOrderUpdate {
        BrokerOrderUpdate {
            broker_order_id: "ord-1".to_owned(),
            account_id: Some("101".to_owned()),
            symbol: symbol.to_owned(),
            side: Some(tv_bot_core_types::TradeSide::Buy),
            quantity: Some(1),
            order_type: Some(EntryOrderType::Limit),
            status: BrokerOrderStatus::Working,
            filled_quantity: 0,
            limit_price: Some(Decimal::new(238_650, 2)),
            stop_price: None,
            average_fill_price: None,
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_market_data_status() -> MarketDataServiceSnapshot {
        MarketDataServiceSnapshot {
            session: tv_bot_market_data::DatabentoSessionStatus {
                market_data: tv_bot_market_data::MarketDataStatusSnapshot {
                    provider: "databento".to_owned(),
                    dataset: "GLBX.MDP3".to_owned(),
                    connection_state: tv_bot_market_data::MarketDataConnectionState::Subscribed,
                    health: tv_bot_market_data::MarketDataHealth::Healthy,
                    feed_statuses: vec![
                        tv_bot_market_data::FeedStatus {
                            instrument_symbol: "GCM2026".to_owned(),
                            feed: tv_bot_core_types::FeedType::Trades,
                            state: tv_bot_market_data::FeedReadinessState::Ready,
                            last_event_at: Some(chrono::Utc::now()),
                            detail: "trade feed ready".to_owned(),
                        },
                        tv_bot_market_data::FeedStatus {
                            instrument_symbol: "GCM2026".to_owned(),
                            feed: tv_bot_core_types::FeedType::Ohlcv1m,
                            state: tv_bot_market_data::FeedReadinessState::Ready,
                            last_event_at: Some(chrono::Utc::now()),
                            detail: "bar feed ready".to_owned(),
                        },
                    ],
                    warmup: WarmupProgress {
                        status: WarmupStatus::Ready,
                        ready_requires_all: true,
                        buffers: vec![tv_bot_market_data::BufferStatus {
                            symbol: "GCM2026".to_owned(),
                            timeframe: tv_bot_core_types::Timeframe::OneMinute,
                            available_bars: 10,
                            required_bars: 10,
                            capacity: 10,
                            ready: true,
                        }],
                        started_at: Some(chrono::Utc::now()),
                        updated_at: chrono::Utc::now(),
                        failure_reason: None,
                    },
                    reconnect_count: 0,
                    last_heartbeat_at: Some(chrono::Utc::now()),
                    last_disconnect_reason: None,
                    updated_at: chrono::Utc::now(),
                },
            },
            warmup_requested: true,
            warmup_mode: tv_bot_market_data::DatabentoWarmupMode::LiveOnly,
            replay_caught_up: true,
            trade_ready: true,
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_storage_status() -> RuntimeStorageStatus {
        RuntimeStorageStatus {
            mode: tv_bot_control_api::RuntimeStorageMode::PrimaryConfigured,
            primary_configured: true,
            sqlite_fallback_enabled: false,
            sqlite_path: PathBuf::from("data/tv_bot_core.sqlite"),
            allow_runtime_fallback: false,
            active_backend: "postgres".to_owned(),
            durable: true,
            fallback_activated: false,
            detail: "primary Postgres persistence is active".to_owned(),
        }
    }

    fn sample_journal_status() -> RuntimeJournalStatus {
        RuntimeJournalStatus {
            backend: "in_memory".to_owned(),
            durable: false,
            detail: "event journal records are retained in memory only".to_owned(),
        }
    }

    fn sample_system_health() -> SystemHealthSnapshot {
        SystemHealthSnapshot {
            cpu_percent: Some(8.0),
            memory_bytes: Some(2_048),
            reconnect_count: 0,
            db_write_latency_ms: Some(2),
            queue_lag_ms: Some(0),
            error_count: 0,
            feed_degraded: false,
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_trade_latency() -> TradePathLatencyRecord {
        let now = chrono::Utc::now();
        TradePathLatencyRecord {
            action_id: "latency-1".to_owned(),
            strategy_id: Some("gc_momentum_fade_v1".to_owned()),
            recorded_at: now,
            timestamps: tv_bot_core_types::TradePathTimestamps {
                market_event_at: None,
                signal_at: None,
                decision_at: Some(now),
                order_sent_at: Some(now),
                broker_ack_at: Some(now),
                fill_at: None,
                sync_update_at: None,
            },
            latency: tv_bot_core_types::TradePathLatencySnapshot {
                signal_latency_ms: None,
                decision_latency_ms: None,
                order_send_latency_ms: Some(0),
                broker_ack_latency_ms: Some(0),
                fill_latency_ms: None,
                sync_update_latency_ms: None,
                end_to_end_fill_latency_ms: None,
                end_to_end_sync_latency_ms: None,
            },
        }
    }

    #[derive(Clone)]
    struct TestAuthApi {
        token: Arc<Mutex<Option<TradovateAccessToken>>>,
    }

    #[derive(Clone)]
    struct TestAccountApi {
        accounts: Arc<Vec<TradovateAccount>>,
    }

    #[derive(Clone)]
    struct TestSyncApi {
        snapshots: Arc<Mutex<VecDeque<TradovateSyncSnapshot>>>,
    }

    #[derive(Clone, Debug, Default)]
    struct TestExecutionApi {
        place_orders: Arc<Mutex<Vec<TradovatePlaceOrderRequest>>>,
        place_osos: Arc<Mutex<Vec<TradovatePlaceOsoRequest>>>,
        liquidations: Arc<Mutex<Vec<TradovateLiquidatePositionRequest>>>,
        cancel_orders: Arc<Mutex<Vec<TradovateCancelOrderRequest>>>,
    }

    #[async_trait]
    impl TradovateAuthApi for TestAuthApi {
        async fn request_access_token(
            &self,
            _request: TradovateAuthRequest,
        ) -> Result<TradovateAccessToken, TradovateError> {
            self.token
                .lock()
                .expect("auth mutex should not poison")
                .clone()
                .ok_or_else(|| TradovateError::AuthTransport {
                    message: "missing token".to_owned(),
                })
        }

        async fn renew_access_token(
            &self,
            _request: tv_bot_broker_tradovate::TradovateRenewAccessTokenRequest,
        ) -> Result<TradovateAccessToken, TradovateError> {
            self.request_access_token(TradovateAuthRequest {
                http_base_url: String::new(),
                environment: tv_bot_core_types::BrokerEnvironment::Demo,
                credentials: sample_credentials(),
            })
            .await
        }
    }

    #[async_trait]
    impl TradovateAccountApi for TestAccountApi {
        async fn list_accounts(
            &self,
            _request: TradovateAccountListRequest,
        ) -> Result<Vec<TradovateAccount>, TradovateError> {
            Ok(self.accounts.as_ref().clone())
        }
    }

    #[async_trait]
    impl TradovateSyncApi for TestSyncApi {
        async fn connect(
            &self,
            _request: TradovateSyncConnectRequest,
        ) -> Result<(), TradovateError> {
            Ok(())
        }

        async fn request_user_sync(
            &self,
            _request: TradovateUserSyncRequest,
        ) -> Result<TradovateSyncSnapshot, TradovateError> {
            self.snapshots
                .lock()
                .expect("sync mutex should not poison")
                .pop_front()
                .ok_or_else(|| TradovateError::SyncTransport {
                    message: "missing sync snapshot".to_owned(),
                })
        }

        async fn next_event(&self) -> Result<Option<TradovateSyncEvent>, TradovateError> {
            Ok(None)
        }

        async fn disconnect(&self) -> Result<(), TradovateError> {
            Ok(())
        }
    }

    #[async_trait]
    impl TradovateExecutionApi for TestExecutionApi {
        async fn place_order(
            &self,
            request: TradovatePlaceOrderRequest,
        ) -> Result<TradovatePlaceOrderResult, TradovateError> {
            self.place_orders
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovatePlaceOrderResult { order_id: 9101 })
        }

        async fn place_oso(
            &self,
            request: TradovatePlaceOsoRequest,
        ) -> Result<TradovatePlaceOsoResult, TradovateError> {
            self.place_osos
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovatePlaceOsoResult {
                order_id: 9102,
                oso1_id: Some(9103),
                oso2_id: Some(9104),
            })
        }

        async fn liquidate_position(
            &self,
            request: TradovateLiquidatePositionRequest,
        ) -> Result<TradovateLiquidatePositionResult, TradovateError> {
            self.liquidations
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovateLiquidatePositionResult { order_id: 9105 })
        }

        async fn cancel_order(
            &self,
            request: TradovateCancelOrderRequest,
        ) -> Result<TradovateCancelOrderResult, TradovateError> {
            self.cancel_orders
                .lock()
                .expect("execution mutex should not poison")
                .push(request.clone());
            Ok(TradovateCancelOrderResult {
                order_id: request.order_id,
            })
        }
    }

    fn sample_credentials() -> TradovateCredentials {
        TradovateCredentials {
            username: "bot-user".to_owned(),
            password: SecretString::new("password".to_owned().into()),
            cid: "cid-123".to_owned(),
            sec: SecretString::new("sec-456".to_owned().into()),
            app_id: "tv-bot-core".to_owned(),
            app_version: "0.1.0".to_owned(),
            device_id: Some("desktop".to_owned()),
        }
    }

    fn sample_token() -> TradovateAccessToken {
        TradovateAccessToken {
            access_token: SecretString::new("access-token".to_owned().into()),
            expiration_time: chrono::DateTime::parse_from_rfc3339("2026-04-10T15:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&chrono::Utc),
            issued_at: chrono::DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&chrono::Utc),
            user_id: Some(7),
            person_id: Some(11),
            market_data_access: Some("realtime".to_owned()),
        }
    }

    fn empty_sync_snapshot() -> TradovateSyncSnapshot {
        TradovateSyncSnapshot {
            occurred_at: chrono::DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&chrono::Utc),
            positions: Vec::new(),
            working_orders: Vec::new(),
            fills: Vec::new(),
            account_snapshot: None,
            mismatch_reason: None,
            detail: "synced".to_owned(),
        }
    }

    async fn sample_session_manager(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        let auth_api = TestAuthApi {
            token: Arc::new(Mutex::new(Some(sample_token()))),
        };
        let account_api = TestAccountApi {
            accounts: Arc::new(vec![TradovateAccount {
                account_id: 101,
                account_name: "paper-primary".to_owned(),
                nickname: None,
                active: true,
            }]),
        };
        let sync_api = TestSyncApi {
            snapshots: Arc::new(Mutex::new(VecDeque::from([empty_sync_snapshot()]))),
        };

        let mut manager = TradovateSessionManager::with_system_clock(
            TradovateSessionConfig::new(
                tv_bot_core_types::BrokerEnvironment::Demo,
                "https://demo.tradovateapi.com/v1",
                "wss://demo.tradovateapi.com/v1/websocket",
            )
            .expect("config should be valid"),
            sample_credentials(),
            TradovateRoutingPreferences {
                paper_account_name: Some("paper-primary".to_owned()),
                live_account_name: None,
            },
            auth_api,
            account_api,
            sync_api,
        )
        .expect("manager should build");

        manager.authenticate().await.expect("auth should succeed");
        manager
            .select_account_for_mode(&RuntimeMode::Paper)
            .await
            .expect("paper account should select");
        manager
            .connect_user_sync()
            .await
            .expect("sync should connect");

        manager
    }

    fn temp_strategy_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("tv_bot_runtime_operator_{unique}.md"))
    }

    fn write_strategy_file(path: &Path) {
        fs::write(
            path,
            include_str!("../../../tests/fixtures/strategies/gc_momentum_fade_v1.md"),
        )
        .expect("strategy file should write");
    }

    #[test]
    fn load_strategy_updates_status_snapshot() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");

        let snapshot = operator.status_snapshot(&sample_context());
        assert!(snapshot.strategy_loaded);
        assert_eq!(
            snapshot
                .current_strategy
                .expect("strategy summary should be present")
                .strategy_id,
            "gc_momentum_fade_v1"
        );
        assert_eq!(
            snapshot.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(snapshot.broker_status.is_some());
        assert_eq!(snapshot.warmup_status, WarmupStatus::Loaded);

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn broker_protection_support_matches_current_tradovate_execution_capabilities() {
        let operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));

        let support = operator.broker_protection_support(&sample_context());

        assert_eq!(
            support,
            BrokerProtectionSupport {
                stop_loss: true,
                take_profit: true,
                trailing_stop: false,
                daily_loss_limit: false,
            }
        );
    }

    #[test]
    fn readiness_requires_override_after_load_and_ready_warmup() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::StartWarmup, &sample_context())
            .expect("warmup should start");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::MarkWarmupReady, &sample_context())
            .expect("warmup should complete");

        let readiness = operator.readiness_snapshot(&sample_context());
        assert!(readiness.report.hard_override_required);
        assert!(readiness
            .report
            .checks
            .iter()
            .any(|check| check.status == ReadinessCheckStatus::Warning));

        let error = operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::Arm {
                    allow_override: false,
                },
                &sample_context(),
            )
            .expect_err("arming should require override");
        assert_eq!(error.status_code(), HttpStatusCode::PreconditionRequired);

        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::Arm {
                    allow_override: true,
                },
                &sample_context(),
            )
            .expect("override path should arm");

        assert_eq!(
            operator.status_snapshot(&sample_context()).arm_state,
            tv_bot_core_types::ArmState::Armed
        );

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn flatten_request_uses_loaded_strategy_and_runtime_guards() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::StartWarmup, &sample_context())
            .expect("warmup should start");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::MarkWarmupReady, &sample_context())
            .expect("warmup should complete");
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::Arm {
                    allow_override: true,
                },
                &sample_context(),
            )
            .expect("override path should arm");

        let tradovate_symbol = operator
            .loaded_strategy
            .as_ref()
            .map(|strategy| {
                strategy
                    .instrument_mapping
                    .as_ref()
                    .map(|mapping| mapping.tradovate_symbol.clone())
                    .unwrap_or_else(|| strategy.compiled.market.market.clone())
            })
            .expect("strategy should be loaded");
        let mut context = sample_context();
        context.open_positions = vec![sample_position("contract:4444", 1)];
        context.working_orders = vec![sample_working_order(&tradovate_symbol)];

        let request = operator
            .build_flatten_request(
                &context,
                ManualCommandSource::Cli,
                12345,
                "manual flatten".to_owned(),
            )
            .expect("flatten request should build");

        match request.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert_eq!(request.mode, RuntimeMode::Paper);
                assert_eq!(
                    request.execution.strategy.metadata.strategy_id,
                    "gc_momentum_fade_v1"
                );
                assert!(request.execution.state.runtime_can_submit_orders);
                assert_eq!(request.execution.instrument.active_contract_id, Some(12345));
                assert!(request.execution.state.current_position.is_some());
                assert_eq!(request.execution.state.working_orders.len(), 1);
                assert_eq!(
                    request.risk_state.current_position,
                    request.execution.state.current_position
                );
                assert_eq!(
                    request.risk_state.unrealized_pnl,
                    Some(Decimal::new(-75, 0))
                );
                assert_eq!(
                    request.execution.intent,
                    ExecutionIntent::Flatten {
                        reason: "manual flatten".to_owned(),
                    }
                );
            }
            other => panic!("unexpected command shape: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn close_position_request_resolves_active_contract_from_context() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");

        let mut context = sample_context();
        context.open_positions = vec![sample_position("contract:4444", 1)];

        let request = operator
            .build_close_position_request(
                &context,
                ManualCommandSource::Dashboard,
                None,
                Some("dashboard close".to_owned()),
            )
            .expect("close-position request should build");

        match request.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert_eq!(request.action_source, ActionSource::Dashboard);
                assert_eq!(request.execution.instrument.active_contract_id, Some(4444));
                assert_eq!(
                    request.execution.intent,
                    ExecutionIntent::Flatten {
                        reason: "dashboard close".to_owned(),
                    }
                );
            }
            other => panic!("unexpected command shape: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn manual_entry_request_uses_loaded_strategy_market_context() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");

        let tradovate_symbol = operator
            .loaded_strategy
            .as_ref()
            .map(|strategy| {
                strategy
                    .instrument_mapping
                    .as_ref()
                    .map(|mapping| mapping.tradovate_symbol.clone())
                    .unwrap_or_else(|| strategy.compiled.market.market.clone())
            })
            .expect("strategy should be loaded");
        let mut context = sample_context();
        context.working_orders = vec![sample_working_order(&tradovate_symbol)];

        let request = operator
            .build_manual_entry_request(
                &context,
                ManualCommandSource::Dashboard,
                TradeSide::Buy,
                2,
                Decimal::new(10, 1),
                Decimal::new(238_510, 2),
                Some(Decimal::new(10, 0)),
                Some("dashboard momentum entry".to_owned()),
            )
            .expect("manual entry request should build");

        match request.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert_eq!(request.execution.instrument.tradovate_symbol, "GCM2026");
                assert_eq!(request.execution.instrument.tick_size, Decimal::new(10, 1));
                assert_eq!(
                    request.execution.instrument.entry_reference_price,
                    Some(Decimal::new(238_510, 2))
                );
                assert_eq!(request.execution.state.working_orders.len(), 1);
                assert_eq!(
                    request.risk_instrument.tick_value_usd,
                    Some(Decimal::new(10, 0))
                );

                assert_eq!(
                    request.execution.intent,
                    ExecutionIntent::Enter {
                        side: TradeSide::Buy,
                        order_type: EntryOrderType::Market,
                        quantity: 2,
                        protective_brackets_expected: true,
                        reason: "dashboard momentum entry".to_owned(),
                    }
                );
            }
            other => panic!("unexpected command shape: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn cancel_working_orders_request_uses_loaded_market_working_orders() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");

        let tradovate_symbol = operator
            .loaded_strategy
            .as_ref()
            .map(|strategy| {
                strategy
                    .instrument_mapping
                    .as_ref()
                    .map(|mapping| mapping.tradovate_symbol.clone())
                    .unwrap_or_else(|| strategy.compiled.market.market.clone())
            })
            .expect("strategy should be loaded");
        let mut context = sample_context();
        context.open_positions = vec![sample_position(&tradovate_symbol, 1)];
        context.working_orders = vec![sample_working_order(&tradovate_symbol)];

        let request = operator
            .build_cancel_working_orders_request(
                &context,
                ManualCommandSource::Cli,
                Some("cancel stale entry".to_owned()),
            )
            .expect("cancel-working-orders request should build");

        match request.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert_eq!(request.mode, RuntimeMode::Paper);
                assert!(request.execution.state.runtime_can_submit_orders);
                assert!(!request.execution.state.new_entries_allowed);
                assert_eq!(request.execution.state.working_orders.len(), 1);
                assert_eq!(
                    request.execution.intent,
                    ExecutionIntent::CancelWorkingOrders {
                        reason: "cancel stale entry".to_owned(),
                    }
                );
            }
            other => panic!("unexpected command shape: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn sanitize_command_request_blocks_new_entries_when_market_data_is_degraded() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");

        let mut context = sample_context();
        if let Some(snapshot) = context.market_data_status.as_mut() {
            snapshot.session.market_data.health = tv_bot_market_data::MarketDataHealth::Degraded;
            snapshot.trade_ready = false;
        }

        let request = operator
            .sanitize_command_request(&context, sample_entry_request())
            .expect("request should sanitize");

        match request.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert!(!request.execution.state.new_entries_allowed);
            }
            other => panic!("unexpected command shape: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn sanitize_command_request_blocks_new_entries_when_operator_gate_is_disabled() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::MarkWarmupReady, &sample_context())
            .expect("warmup should mark ready");
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::Arm {
                    allow_override: true,
                },
                &sample_context(),
            )
            .expect("runtime should arm");
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::SetNewEntriesEnabled {
                    enabled: false,
                    reason: Some("let the current runner finish without adding".to_owned()),
                },
                &sample_context(),
            )
            .expect("operator gate should update");

        let request = operator
            .sanitize_command_request(&sample_context(), sample_entry_request())
            .expect("request should sanitize");

        match request.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert!(request.execution.state.runtime_can_submit_orders);
                assert!(!request.execution.state.new_entries_allowed);
            }
            other => panic!("unexpected command shape: {other:?}"),
        }

        let readiness = operator.readiness_snapshot(&sample_context());
        assert!(readiness.report.checks.iter().any(|check| {
            check.name == "operator_entry_gate"
                && check.status == ReadinessCheckStatus::Warning
                && check
                    .message
                    .contains("new entries are disabled by operator control")
        }));

        let _ = fs::remove_file(strategy_path);
    }

    #[test]
    fn readiness_requires_override_when_primary_storage_falls_back_to_sqlite() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::StartWarmup, &sample_context())
            .expect("warmup should start");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::MarkWarmupReady, &sample_context())
            .expect("warmup should complete");

        let mut context = sample_context();
        context.storage_status.fallback_activated = true;
        context.storage_status.allow_runtime_fallback = true;
        context.storage_status.detail =
            "primary Postgres persistence is unavailable; SQLite fallback is active".to_owned();

        let readiness = operator.readiness_snapshot(&context);
        assert!(readiness.report.hard_override_required);
        assert!(readiness.report.checks.iter().any(|check| {
            check.name == "storage" && check.status == ReadinessCheckStatus::Warning
        }));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_entry_request_dispatches_broker_side_brackets_through_runtime_control_loop() {
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let mut operator = RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Paper));
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::LoadStrategy {
                    path: strategy_path.clone(),
                },
                &sample_context(),
            )
            .expect("strategy should load");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::StartWarmup, &sample_context())
            .expect("warmup should start");
        operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::MarkWarmupReady, &sample_context())
            .expect("warmup should complete");
        operator
            .apply_lifecycle_command(
                RuntimeLifecycleCommand::Arm {
                    allow_override: true,
                },
                &sample_context(),
            )
            .expect("override path should arm");

        let mut request = sample_entry_request();
        if let ControlApiCommand::ManualIntent { request, .. } = &mut request.command {
            request.risk_instrument.tick_value_usd = Some(Decimal::ONE);
            request.execution.intent = ExecutionIntent::Enter {
                side: tv_bot_core_types::TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: true,
                reason: "paper acceptance entry".to_owned(),
            };
        }

        let sanitized = operator
            .sanitize_command_request(&sample_context(), request)
            .expect("request should sanitize");

        let runtime_request = match sanitized.command {
            ControlApiCommand::ManualIntent { request, .. } => {
                assert_eq!(request.mode, RuntimeMode::Paper);
                assert!(request.execution.state.runtime_can_submit_orders);
                assert!(request.execution.state.new_entries_allowed);
                request
            }
            other => panic!("unexpected command shape: {other:?}"),
        };
        let expected_symbol = runtime_request
            .execution
            .instrument
            .tradovate_symbol
            .clone();

        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let outcome = RuntimeControlLoop::handle_command(
            RuntimeCommand::ManualIntent(runtime_request),
            &mut manager,
            &execution_api,
            &journal,
        )
        .await
        .expect("manual paper entry should dispatch");

        let tv_bot_runtime_kernel::RuntimeCommandOutcome::Execution(outcome) = outcome;
        assert_eq!(outcome.risk.decision.status, RiskDecisionStatus::Accepted);
        assert!(outcome.dispatch.is_some());
        let approved_quantity = outcome
            .risk
            .approved_quantity
            .expect("risk sizing should approve a quantity");

        let place_orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_orders.is_empty());
        drop(place_orders);

        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert!(liquidations.is_empty());
        drop(liquidations);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(place_osos.len(), 1);
        let oso = &place_osos[0];
        assert_eq!(oso.context.account_id, 101);
        assert_eq!(oso.context.account_spec, "paper-primary");
        assert_eq!(oso.order.symbol, expected_symbol);
        assert_eq!(oso.order.quantity, approved_quantity);
        assert_eq!(
            oso.order.order_type,
            tv_bot_broker_tradovate::TradovateOrderType::Market
        );
        assert_eq!(oso.order.brackets.len(), 2);
        assert!(oso.order.brackets.iter().any(|bracket| {
            bracket.text.as_deref() == Some("stop_loss") && bracket.stop_price.is_some()
        }));
        assert!(oso.order.brackets.iter().any(|bracket| {
            bracket.text.as_deref() == Some("take_profit") && bracket.limit_price.is_some()
        }));

        let journal_actions = journal
            .list()
            .expect("journal should list records")
            .into_iter()
            .map(|record| record.action)
            .collect::<Vec<_>>();
        assert!(
            journal_actions.starts_with(&["intent_received".to_owned(), "decision".to_owned(),])
        );
        assert!(journal_actions
            .iter()
            .any(|action| action == "hard_override_used"));
        assert!(journal_actions
            .iter()
            .any(|action| action == "dispatch_succeeded"));

        let _ = fs::remove_file(strategy_path);
    }

    fn sample_entry_request() -> HttpCommandRequest {
        HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source: ManualCommandSource::Cli,
                request: RuntimeExecutionRequest {
                    mode: RuntimeMode::Paper,
                    action_source: ManualCommandSource::Cli.into(),
                    authenticated_operator: None,
                    execution: ExecutionRequest {
                        strategy: compile_loaded_strategy(
                            PathBuf::from("strategy.md"),
                            StrictStrategyCompiler
                                .compile_markdown(include_str!(
                                    "../../../tests/fixtures/strategies/gc_momentum_fade_v1.md"
                                ))
                                .expect("strategy should compile"),
                        )
                        .compiled,
                        instrument: ExecutionInstrumentContext {
                            tradovate_symbol: "GCM2026".to_owned(),
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Some(Decimal::new(238_510, 2)),
                            active_contract_id: Some(4444),
                        },
                        state: ExecutionStateContext {
                            runtime_can_submit_orders: true,
                            new_entries_allowed: true,
                            current_position: None,
                            working_orders: Vec::new(),
                        },
                        intent: ExecutionIntent::Enter {
                            side: tv_bot_core_types::TradeSide::Buy,
                            order_type: EntryOrderType::Market,
                            quantity: 1,
                            protective_brackets_expected: false,
                            reason: "operator-test".to_owned(),
                        },
                    },
                    risk_instrument: RiskInstrumentContext::default(),
                    risk_state: RiskStateContext {
                        trades_today: 0,
                        consecutive_losses: 0,
                        current_position: None,
                        unrealized_pnl: None,
                        broker_support: BrokerProtectionSupport::default(),
                        hard_override_active: false,
                    },
                },
            },
        }
    }
}
