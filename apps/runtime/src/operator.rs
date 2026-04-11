use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use rust_decimal::Decimal;
use thiserror::Error;
use tv_bot_control_api::{
    ControlApiCommand, HttpCommandRequest, HttpStatusCode, LoadedStrategySummary,
    ManualCommandSource, RuntimeJournalStatus, RuntimeLifecycleCommand, RuntimeReadinessSnapshot,
    RuntimeReconnectReviewStatus, RuntimeShutdownReviewStatus, RuntimeStatusSnapshot,
    RuntimeStorageStatus,
};
use tv_bot_core_types::{
    BrokerOrderStatus, BrokerOrderUpdate, BrokerPositionSnapshot, BrokerStatusSnapshot,
    ExecutionIntent, InstrumentMapping, ReadinessCheckStatus, RuntimeMode, SystemHealthSnapshot,
    TradePathLatencyRecord, WarmupStatus,
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
            RuntimeLifecycleCommand::ResolveReconnectReview { .. } => {
                Ok("reconnect review prepared".to_owned())
            }
            RuntimeLifecycleCommand::Shutdown { .. } => Ok("shutdown review prepared".to_owned()),
            RuntimeLifecycleCommand::Flatten { .. } => Ok("flatten prepared".to_owned()),
        }
    }

    pub fn build_flatten_request(
        &self,
        context: &RuntimeStatusContext,
        source: ManualCommandSource,
        contract_id: i64,
        reason: String,
    ) -> Result<HttpCommandRequest, RuntimeOperatorError> {
        let strategy = self.loaded_strategy()?;
        let tradovate_symbol = strategy
            .instrument_mapping
            .as_ref()
            .map(|mapping| mapping.tradovate_symbol.clone())
            .unwrap_or_else(|| strategy.compiled.market.market.clone());
        let current_position =
            self.active_position_for_symbol(context, &tradovate_symbol, Some(contract_id));

        Ok(HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source,
                request: RuntimeExecutionRequest {
                    mode: self.runtime.current_mode(),
                    action_source: source.into(),
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
        !matches!(
            self.market_data_health(context),
            DependencyHealth::Blocking(_)
        ) && !matches!(
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
    let resolver = FrontMonthResolver::with_system_clock(StaticContractChainProvider::new());
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use tv_bot_control_api::RuntimeLifecycleCommand;
    use tv_bot_core_types::EntryOrderType;

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
            include_str!("../../../strategies/examples/gc_momentum_fade_v1.md"),
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
        context.open_positions = vec![sample_position(&tradovate_symbol, 1)];
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

    fn sample_entry_request() -> HttpCommandRequest {
        HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source: ManualCommandSource::Cli,
                request: RuntimeExecutionRequest {
                    mode: RuntimeMode::Paper,
                    action_source: ManualCommandSource::Cli.into(),
                    execution: ExecutionRequest {
                        strategy: compile_loaded_strategy(
                            PathBuf::from("strategy.md"),
                            StrictStrategyCompiler
                                .compile_markdown(include_str!(
                                    "../../../strategies/examples/gc_momentum_fade_v1.md"
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
