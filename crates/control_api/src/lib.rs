//! Transport-agnostic local control-plane command routing.

mod http;
mod websocket;

use std::path::PathBuf;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tv_bot_broker_tradovate::{
    Clock as TradovateClock, TradovateAccountApi, TradovateAuthApi, TradovateExecutionApi,
    TradovateSessionManager, TradovateSyncApi,
};
use tv_bot_core_types::{
    ActionSource, ArmReadinessReport, ArmState, BrokerFillUpdate, BrokerOrderUpdate,
    BrokerPositionSnapshot, BrokerStatusSnapshot, EventJournalRecord, InstrumentMapping,
    OperatorRole, RiskDecisionStatus, RuntimeMode, SystemHealthSnapshot, Timeframe,
    TradePathLatencyRecord, TradeSide, WarmupStatus,
};
use tv_bot_execution_engine::{ExecutionDispatchError, ExecutionEngineError};
use tv_bot_journal::EventJournal;
use tv_bot_market_data::{MarketDataConnectionState, MarketDataHealth, MarketDataServiceSnapshot};
use tv_bot_runtime_kernel::{
    RuntimeCommand, RuntimeCommandError, RuntimeCommandOutcome, RuntimeControlLoop,
    RuntimeExecutionError, RuntimeExecutionRequest,
};
use tv_bot_state_store::ProjectedTradingHistoryState;

pub use http::{
    HttpCommandHandler, HttpCommandRequest, HttpCommandResponse, HttpResponseBody, HttpStatusCode,
};
pub use websocket::{
    ControlApiEvent, ControlApiEventPublisher, NoopEventPublisher, WebSocketEventHub,
    WebSocketEventHubError, WebSocketEventStream, WebSocketEventStreamError,
};

pub const MODULE_STATUS: &str = "implemented";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualCommandSource {
    Dashboard,
    Cli,
}

impl ManualCommandSource {
    pub fn action_source(self) -> ActionSource {
        match self {
            Self::Dashboard => ActionSource::Dashboard,
            Self::Cli => ActionSource::Cli,
        }
    }
}

impl From<ManualCommandSource> for ActionSource {
    fn from(value: ManualCommandSource) -> Self {
        value.action_source()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlApiCommand {
    ManualIntent {
        source: ManualCommandSource,
        request: RuntimeExecutionRequest,
    },
    StrategyIntent {
        request: RuntimeExecutionRequest,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlApiCommandStatus {
    Executed,
    Rejected,
    RequiresOverride,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlApiCommandResult {
    pub status: ControlApiCommandStatus,
    pub risk_status: RiskDecisionStatus,
    pub dispatch_performed: bool,
    pub reason: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadedStrategySummary {
    pub path: PathBuf,
    pub title: Option<String>,
    pub strategy_id: String,
    pub name: String,
    pub version: String,
    pub market_family: String,
    pub warning_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStorageMode {
    Unconfigured,
    PrimaryConfigured,
    SqliteFallbackOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStorageStatus {
    pub mode: RuntimeStorageMode,
    pub primary_configured: bool,
    pub sqlite_fallback_enabled: bool,
    pub sqlite_path: PathBuf,
    pub allow_runtime_fallback: bool,
    pub active_backend: String,
    pub durable: bool,
    pub fallback_activated: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeJournalStatus {
    pub backend: String,
    pub durable: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAuthenticatedOperatorSnapshot {
    pub user_id: String,
    pub display_name: Option<String>,
    pub session_id: Option<String>,
    pub device_id: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub roles: Vec<OperatorRole>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAuthorizationSnapshot {
    pub can_view: bool,
    pub can_manage_runtime: bool,
    pub can_manage_strategies: bool,
    pub can_update_settings: bool,
    pub can_trade: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStatusSnapshot {
    pub mode: RuntimeMode,
    pub arm_state: ArmState,
    pub warmup_status: WarmupStatus,
    pub strategy_loaded: bool,
    pub hard_override_active: bool,
    pub operator_new_entries_enabled: bool,
    pub operator_new_entries_reason: Option<String>,
    pub current_strategy: Option<LoadedStrategySummary>,
    pub broker_status: Option<BrokerStatusSnapshot>,
    pub market_data_status: Option<MarketDataServiceSnapshot>,
    pub market_data_detail: Option<String>,
    pub storage_status: RuntimeStorageStatus,
    pub journal_status: RuntimeJournalStatus,
    pub system_health: Option<SystemHealthSnapshot>,
    pub latest_trade_latency: Option<TradePathLatencyRecord>,
    pub recorded_trade_latency_count: usize,
    pub current_account_name: Option<String>,
    pub authenticated_operator: Option<RuntimeAuthenticatedOperatorSnapshot>,
    pub authorization: RuntimeAuthorizationSnapshot,
    pub instrument_mapping: Option<InstrumentMapping>,
    pub instrument_resolution_error: Option<String>,
    pub reconnect_review: RuntimeReconnectReviewStatus,
    pub shutdown_review: RuntimeShutdownReviewStatus,
    pub http_bind: String,
    pub websocket_bind: String,
    pub command_dispatch_ready: bool,
    pub command_dispatch_detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReadinessSnapshot {
    pub status: RuntimeStatusSnapshot,
    pub report: ArmReadinessReport,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHistorySnapshot {
    pub projection: ProjectedTradingHistoryState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeJournalSnapshot {
    pub total_records: usize,
    pub records: Vec<EventJournalRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeChartInstrumentSummary {
    pub strategy_id: String,
    pub strategy_name: String,
    pub market_family: String,
    pub market_display_name: Option<String>,
    pub tradovate_symbol: Option<String>,
    pub canonical_symbol: Option<String>,
    pub databento_symbols: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeChartConfigResponse {
    pub available: bool,
    pub detail: String,
    pub sample_data_active: bool,
    pub instrument: Option<RuntimeChartInstrumentSummary>,
    pub supported_timeframes: Vec<Timeframe>,
    pub default_timeframe: Option<Timeframe>,
    pub market_data_connection_state: Option<MarketDataConnectionState>,
    pub market_data_health: Option<MarketDataHealth>,
    pub replay_caught_up: bool,
    pub trade_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeChartBar {
    pub timeframe: Timeframe,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: u64,
    pub closed_at: chrono::DateTime<chrono::Utc>,
    pub is_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeChartSnapshot {
    pub config: RuntimeChartConfigResponse,
    pub timeframe: Timeframe,
    pub requested_limit: usize,
    pub bars: Vec<RuntimeChartBar>,
    pub latest_price: Option<Decimal>,
    pub latest_closed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub active_position: Option<BrokerPositionSnapshot>,
    pub working_orders: Vec<BrokerOrderUpdate>,
    pub recent_fills: Vec<BrokerFillUpdate>,
    pub can_load_older_history: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeChartHistoryResponse {
    pub config: RuntimeChartConfigResponse,
    pub timeframe: Timeframe,
    pub requested_limit: usize,
    pub before: Option<chrono::DateTime<chrono::Utc>>,
    pub bars: Vec<RuntimeChartBar>,
    pub can_load_older_history: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeChartStreamEvent {
    Snapshot {
        snapshot: RuntimeChartSnapshot,
        occurred_at: chrono::DateTime<chrono::Utc>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStrategyIssueSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStrategyIssue {
    pub severity: RuntimeStrategyIssueSeverity,
    pub message: String,
    pub section: Option<String>,
    pub field: Option<String>,
    pub line: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStrategyCatalogEntry {
    pub path: PathBuf,
    pub display_path: String,
    pub valid: bool,
    pub title: Option<String>,
    pub strategy_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub market_family: Option<String>,
    pub warning_count: usize,
    pub error_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStrategyLibraryResponse {
    pub scanned_roots: Vec<PathBuf>,
    pub strategies: Vec<RuntimeStrategyCatalogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStrategyValidationRequest {
    pub source: ManualCommandSource,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStrategyUploadRequest {
    pub source: ManualCommandSource,
    pub filename: String,
    pub markdown: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStrategyValidationResponse {
    pub path: PathBuf,
    pub display_path: String,
    pub valid: bool,
    pub title: Option<String>,
    pub summary: Option<LoadedStrategySummary>,
    pub warnings: Vec<RuntimeStrategyIssue>,
    pub errors: Vec<RuntimeStrategyIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEditableSettings {
    pub startup_mode: RuntimeMode,
    pub default_strategy_path: Option<PathBuf>,
    pub allow_sqlite_fallback: bool,
    pub paper_account_name: Option<String>,
    pub live_account_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSettingsPersistenceMode {
    SessionOnly,
    ConfigFile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsSnapshot {
    pub editable: RuntimeEditableSettings,
    pub http_bind: String,
    pub websocket_bind: String,
    pub config_file_path: Option<PathBuf>,
    pub persistence_mode: RuntimeSettingsPersistenceMode,
    pub restart_required: bool,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsUpdateRequest {
    pub source: ManualCommandSource,
    pub settings: RuntimeEditableSettings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsUpdateResponse {
    pub message: String,
    pub settings: RuntimeSettingsSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeReconnectDecision {
    ClosePosition,
    LeaveBrokerProtected,
    ReattachBotManagement,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeReconnectReviewStatus {
    pub required: bool,
    pub reason: Option<String>,
    pub last_decision: Option<RuntimeReconnectDecision>,
    pub open_position_count: usize,
    pub working_order_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeShutdownDecision {
    FlattenFirst,
    LeaveBrokerProtected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShutdownReviewStatus {
    pub pending_signal: bool,
    pub blocked: bool,
    pub awaiting_flatten: bool,
    pub decision: Option<RuntimeShutdownDecision>,
    pub reason: Option<String>,
    pub open_position_count: usize,
    pub all_positions_broker_protected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeLifecycleCommand {
    SetMode {
        mode: RuntimeMode,
    },
    LoadStrategy {
        path: PathBuf,
    },
    StartWarmup,
    MarkWarmupReady,
    MarkWarmupFailed {
        reason: Option<String>,
    },
    Arm {
        allow_override: bool,
    },
    Disarm,
    Pause,
    Resume,
    SetNewEntriesEnabled {
        enabled: bool,
        reason: Option<String>,
    },
    ResolveReconnectReview {
        decision: RuntimeReconnectDecision,
        contract_id: Option<i64>,
        reason: Option<String>,
    },
    Shutdown {
        decision: RuntimeShutdownDecision,
        contract_id: Option<i64>,
        reason: Option<String>,
    },
    ClosePosition {
        contract_id: Option<i64>,
        reason: Option<String>,
    },
    ManualEntry {
        side: TradeSide,
        quantity: u32,
        tick_size: Decimal,
        entry_reference_price: Decimal,
        tick_value_usd: Option<Decimal>,
        reason: Option<String>,
    },
    CancelWorkingOrders {
        reason: Option<String>,
    },
    Flatten {
        contract_id: i64,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLifecycleRequest {
    pub source: ManualCommandSource,
    pub command: RuntimeLifecycleCommand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLifecycleResponse {
    pub status_code: HttpStatusCode,
    pub message: String,
    pub status: RuntimeStatusSnapshot,
    pub readiness: RuntimeReadinessSnapshot,
    pub command_result: Option<ControlApiCommandResult>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ControlApiError {
    #[error("runtime command failed: {source}")]
    Runtime { source: RuntimeCommandError },
}

impl ControlApiError {
    pub fn status_code(&self) -> HttpStatusCode {
        match self {
            Self::Runtime { source } => status_code_for_runtime_error(source),
        }
    }
}

#[async_trait]
pub trait RuntimeCommandDispatcher {
    async fn dispatch(
        &mut self,
        command: RuntimeCommand,
    ) -> Result<RuntimeCommandOutcome, RuntimeCommandError>;
}

pub struct LocalControlApi<D> {
    dispatcher: D,
}

impl<D> LocalControlApi<D> {
    pub fn new(dispatcher: D) -> Self {
        Self { dispatcher }
    }

    pub fn dispatcher(&self) -> &D {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut D {
        &mut self.dispatcher
    }

    pub fn into_dispatcher(self) -> D {
        self.dispatcher
    }
}

impl<D> LocalControlApi<D>
where
    D: RuntimeCommandDispatcher,
{
    pub async fn handle_command(
        &mut self,
        command: ControlApiCommand,
    ) -> Result<ControlApiCommandResult, ControlApiError> {
        let runtime_command = normalize_command(command);
        let outcome = self
            .dispatcher
            .dispatch(runtime_command)
            .await
            .map_err(|source| ControlApiError::Runtime { source })?;

        Ok(match outcome {
            RuntimeCommandOutcome::Execution(outcome) => {
                let mut warnings = outcome.risk.decision.warnings.clone();
                if let Some(dispatch) = &outcome.dispatch {
                    warnings.extend(dispatch.warnings.iter().cloned());
                }

                let status = match outcome.risk.decision.status {
                    RiskDecisionStatus::Accepted => ControlApiCommandStatus::Executed,
                    RiskDecisionStatus::Rejected => ControlApiCommandStatus::Rejected,
                    RiskDecisionStatus::RequiresOverride => {
                        ControlApiCommandStatus::RequiresOverride
                    }
                };

                ControlApiCommandResult {
                    status,
                    risk_status: outcome.risk.decision.status,
                    dispatch_performed: outcome.dispatch.is_some(),
                    reason: outcome.risk.decision.reason,
                    warnings,
                }
            }
        })
    }
}

pub struct RuntimeKernelCommandDispatcher<A, B, C, Clk, E, J> {
    session: TradovateSessionManager<A, B, C, Clk>,
    execution_api: E,
    journal: J,
}

impl<A, B, C, Clk, E, J> RuntimeKernelCommandDispatcher<A, B, C, Clk, E, J> {
    pub fn new(
        session: TradovateSessionManager<A, B, C, Clk>,
        execution_api: E,
        journal: J,
    ) -> Self {
        Self {
            session,
            execution_api,
            journal,
        }
    }

    pub fn session(&self) -> &TradovateSessionManager<A, B, C, Clk> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut TradovateSessionManager<A, B, C, Clk> {
        &mut self.session
    }

    pub fn execution_api(&self) -> &E {
        &self.execution_api
    }

    pub fn journal(&self) -> &J {
        &self.journal
    }
}

#[async_trait]
impl<A, B, C, Clk, E, J> RuntimeCommandDispatcher
    for RuntimeKernelCommandDispatcher<A, B, C, Clk, E, J>
where
    A: TradovateAuthApi + Send,
    B: TradovateAccountApi + Send,
    C: TradovateSyncApi + Send,
    Clk: TradovateClock + Send,
    E: TradovateExecutionApi + Send + Sync,
    J: EventJournal + Send + Sync,
{
    async fn dispatch(
        &mut self,
        command: RuntimeCommand,
    ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
        RuntimeControlLoop::handle_command(
            command,
            &mut self.session,
            &self.execution_api,
            &self.journal,
        )
        .await
    }
}

fn normalize_command(command: ControlApiCommand) -> RuntimeCommand {
    match command {
        ControlApiCommand::ManualIntent {
            source,
            mut request,
        } => {
            request.action_source = source.action_source();
            RuntimeCommand::ManualIntent(request)
        }
        ControlApiCommand::StrategyIntent { mut request } => {
            request.action_source = ActionSource::System;
            RuntimeCommand::StrategyIntent(request)
        }
    }
}

fn status_code_for_runtime_error(error: &RuntimeCommandError) -> HttpStatusCode {
    match error {
        RuntimeCommandError::Unavailable { .. } => HttpStatusCode::Conflict,
        RuntimeCommandError::Journal { .. } => HttpStatusCode::InternalServerError,
        RuntimeCommandError::Execution { source } => match source {
            RuntimeExecutionError::Journal { .. } => HttpStatusCode::InternalServerError,
            RuntimeExecutionError::Dispatch { source } => match source {
                ExecutionDispatchError::Planning { source } => {
                    status_code_for_execution_planning_error(source)
                }
                ExecutionDispatchError::Broker { .. } => HttpStatusCode::InternalServerError,
            },
        },
    }
}

fn status_code_for_execution_planning_error(error: &ExecutionEngineError) -> HttpStatusCode {
    match error {
        ExecutionEngineError::OrderPlacementBlocked => HttpStatusCode::PreconditionRequired,
        ExecutionEngineError::NewEntriesBlocked
        | ExecutionEngineError::InvalidTickSize
        | ExecutionEngineError::InvalidEntryQuantity
        | ExecutionEngineError::InvalidReductionQuantity
        | ExecutionEngineError::MissingEntryReferencePrice
        | ExecutionEngineError::MissingProtectiveReferencePrice
        | ExecutionEngineError::MissingOpenPosition { .. }
        | ExecutionEngineError::MissingContractId { .. }
        | ExecutionEngineError::MissingWorkingOrders { .. }
        | ExecutionEngineError::InvalidWorkingOrderId { .. }
        | ExecutionEngineError::UnsupportedEntryOrderType { .. }
        | ExecutionEngineError::ScaleInDisabled
        | ExecutionEngineError::ScaleInMaxLegsReached
        | ExecutionEngineError::UnsupportedIntent { .. }
        | ExecutionEngineError::UnsupportedBrokerRequiredFeature { .. }
        | ExecutionEngineError::MissingRequiredProtectiveBracket => HttpStatusCode::Conflict,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use tv_bot_core_types::{
        ActionSource, BreakEvenRule, BrokerPreference, CompiledStrategy, ContractMode,
        DailyLossLimit, DashboardDisplay, DataFeedRequirement, DataRequirements, EntryOrderType,
        EntryRules, ExecutionIntent, ExecutionSpec, ExitRules, FailsafeRules, FeedType,
        MarketConfig, MarketSelection, PartialTakeProfitRule, PositionSizing, PositionSizingMode,
        ReversalMode, RiskDecision, RiskDecisionStatus, RiskLimits, ScalingConfig, SessionMode,
        SessionRules, SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata,
        Timeframe, TradeManagement, TrailingRule,
    };
    use tv_bot_execution_engine::{
        ExecutionDispatchError, ExecutionDispatchReport, ExecutionDispatchResult,
        ExecutionEngineError, ExecutionInstrumentContext, ExecutionRequest, ExecutionStateContext,
    };
    use tv_bot_risk_engine::{RiskEvaluationOutcome, RiskInstrumentContext, RiskStateContext};
    use tv_bot_runtime_kernel::{RuntimeExecutionOutcome, RuntimeExecutionRequest};

    use super::*;

    #[derive(Debug)]
    struct FakeDispatcher {
        commands: Vec<RuntimeCommand>,
        next_result: Option<Result<RuntimeCommandOutcome, RuntimeCommandError>>,
    }

    #[async_trait]
    impl RuntimeCommandDispatcher for FakeDispatcher {
        async fn dispatch(
            &mut self,
            command: RuntimeCommand,
        ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
            self.commands.push(command);
            self.next_result
                .take()
                .expect("fake dispatcher should have a queued result")
        }
    }

    fn sample_request() -> RuntimeExecutionRequest {
        RuntimeExecutionRequest {
            mode: tv_bot_core_types::RuntimeMode::Paper,
            action_source: ActionSource::Cli,
            authenticated_operator: None,
            execution: ExecutionRequest {
                strategy: sample_strategy(),
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
                    reason: "api-entry".to_owned(),
                },
            },
            risk_instrument: RiskInstrumentContext {
                tick_value_usd: Some(Decimal::new(10, 0)),
            },
            risk_state: RiskStateContext {
                trades_today: 0,
                consecutive_losses: 0,
                current_position: None,
                unrealized_pnl: Some(Decimal::ZERO),
                broker_support: tv_bot_risk_engine::BrokerProtectionSupport {
                    stop_loss: true,
                    take_profit: true,
                    trailing_stop: true,
                    daily_loss_limit: true,
                },
                hard_override_active: false,
            },
        }
    }

    fn sample_strategy() -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "gc_control_api_v1".to_owned(),
                name: "GC Control API".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "control api tests".to_owned(),
                tags: Vec::new(),
                source: None,
                notes: None,
            },
            market: MarketConfig {
                market: "gold".to_owned(),
                selection: MarketSelection {
                    contract_mode: ContractMode::FrontMonthAuto,
                },
            },
            session: SessionRules {
                mode: SessionMode::Always,
                timezone: "America/New_York".to_owned(),
                trade_window: None,
                no_new_entries_after: None,
                flatten_rule: None,
                allowed_days: Vec::new(),
            },
            data_requirements: DataRequirements {
                feeds: vec![DataFeedRequirement {
                    kind: FeedType::Trades,
                }],
                timeframes: vec![Timeframe::OneMinute],
                multi_timeframe: false,
                requires: None,
            },
            warmup: tv_bot_core_types::WarmupRequirements {
                bars_required: BTreeMap::from([(Timeframe::OneMinute, 10)]),
                ready_requires_all: true,
            },
            signal_confirmation: SignalConfirmation {
                mode: SignalCombinationMode::All,
                primary_conditions: vec!["trend".to_owned()],
                n_required: None,
                secondary_conditions: Vec::new(),
                score_threshold: None,
                regime_filter: None,
                sequence: Vec::new(),
            },
            entry_rules: EntryRules {
                long_enabled: true,
                short_enabled: true,
                entry_order_type: EntryOrderType::Market,
                entry_conditions: None,
                max_entry_distance_ticks: None,
                entry_timeout_seconds: None,
                allow_reentry_same_bar: None,
                entry_filters: None,
            },
            exit_rules: ExitRules {
                exit_on_opposite_signal: false,
                flatten_on_session_end: true,
                exit_conditions: Vec::new(),
                timeout_exit: None,
                max_hold_seconds: None,
                emergency_exit_rules: None,
            },
            position_sizing: PositionSizing {
                mode: PositionSizingMode::Fixed,
                contracts: Some(1),
                max_risk_usd: None,
                min_contracts: None,
                max_contracts: None,
                fallback_fixed_contracts: Some(1),
                rounding_mode: None,
            },
            execution: ExecutionSpec {
                reversal_mode: ReversalMode::FlattenFirst,
                scaling: ScalingConfig {
                    allow_scale_in: false,
                    allow_scale_out: false,
                    max_legs: 1,
                },
                broker_preferences: tv_bot_core_types::BrokerPreferences {
                    stop_loss: BrokerPreference::BotAllowed,
                    take_profit: BrokerPreference::BotAllowed,
                    trailing_stop: BrokerPreference::BotAllowed,
                },
            },
            trade_management: TradeManagement {
                initial_stop_ticks: 10,
                take_profit_ticks: 20,
                break_even: Some(BreakEvenRule {
                    enabled: true,
                    activate_at_ticks: Some(12),
                }),
                trailing: Some(TrailingRule {
                    enabled: true,
                    activate_at_ticks: Some(18),
                    trail_ticks: Some(6),
                }),
                partial_take_profit: Some(PartialTakeProfitRule {
                    enabled: false,
                    targets: Vec::new(),
                }),
                post_entry_rules: None,
                time_based_adjustments: None,
            },
            risk: RiskLimits {
                daily_loss: DailyLossLimit {
                    broker_side_required: false,
                    local_backup_enabled: true,
                },
                max_trades_per_day: 3,
                max_consecutive_losses: 2,
                max_open_positions: Some(1),
                max_unrealized_drawdown_usd: Some(Decimal::new(500, 0)),
                cooldown_after_daily_stop: None,
                max_notional_exposure: None,
            },
            failsafes: FailsafeRules {
                no_new_entries_on_data_degrade: true,
                pause_on_broker_sync_mismatch: true,
                pause_on_reconnect_until_reviewed: Some(true),
                kill_on_repeated_order_rejects: None,
                abnormal_spread_guard: None,
                clock_sanity_required: Some(true),
                storage_health_required: Some(true),
            },
            state_behavior: StateBehavior {
                cooldown_after_loss_s: 120,
                max_reentries_per_side: 1,
                regime_mode: None,
                memory_reset_rules: None,
                post_win_cooldown_s: None,
                failed_setup_decay: None,
                reentry_logic: None,
            },
            dashboard_display: DashboardDisplay {
                show: vec!["pnl".to_owned()],
                default_overlay: "entries".to_owned(),
                debug_panels: Vec::new(),
                custom_labels: None,
                preferred_chart_timeframe: None,
            },
        }
    }

    fn sample_execution_outcome(
        risk_status: RiskDecisionStatus,
        dispatch_performed: bool,
    ) -> RuntimeCommandOutcome {
        RuntimeCommandOutcome::Execution(RuntimeExecutionOutcome {
            risk: RiskEvaluationOutcome {
                decision: RiskDecision {
                    status: risk_status,
                    reason: match risk_status {
                        RiskDecisionStatus::Accepted => "risk checks passed".to_owned(),
                        RiskDecisionStatus::Rejected => "risk rejected".to_owned(),
                        RiskDecisionStatus::RequiresOverride => "override required".to_owned(),
                    },
                    warnings: vec!["warning-1".to_owned()],
                },
                adjusted_intent: ExecutionIntent::PauseStrategy {
                    reason: "pause".to_owned(),
                },
                approved_quantity: None,
                hard_override_reasons: if risk_status == RiskDecisionStatus::RequiresOverride {
                    vec!["override".to_owned()]
                } else {
                    Vec::new()
                },
            },
            dispatch: dispatch_performed.then(|| ExecutionDispatchReport {
                results: vec![ExecutionDispatchResult::StrategyPaused {
                    reason: "pause".to_owned(),
                }],
                warnings: vec!["dispatch-warning".to_owned()],
            }),
        })
    }

    #[tokio::test]
    async fn manual_dashboard_command_maps_to_runtime_manual_intent() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Ok(sample_execution_outcome(
                RiskDecisionStatus::Accepted,
                true,
            ))),
        };
        let mut api = LocalControlApi::new(dispatcher);

        let result = api
            .handle_command(ControlApiCommand::ManualIntent {
                source: ManualCommandSource::Dashboard,
                request: sample_request(),
            })
            .await
            .expect("manual command should succeed");

        assert_eq!(result.status, ControlApiCommandStatus::Executed);
        assert!(result.dispatch_performed);
        assert_eq!(
            result.warnings,
            vec!["warning-1".to_owned(), "dispatch-warning".to_owned()]
        );

        let dispatcher = api.into_dispatcher();
        assert_eq!(dispatcher.commands.len(), 1);
        match &dispatcher.commands[0] {
            RuntimeCommand::ManualIntent(request) => {
                assert_eq!(request.action_source, ActionSource::Dashboard);
            }
            other => panic!("unexpected runtime command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn strategy_command_maps_to_runtime_strategy_intent_with_system_source() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Ok(sample_execution_outcome(
                RiskDecisionStatus::Accepted,
                true,
            ))),
        };
        let mut api = LocalControlApi::new(dispatcher);

        let result = api
            .handle_command(ControlApiCommand::StrategyIntent {
                request: sample_request(),
            })
            .await
            .expect("strategy command should succeed");

        assert_eq!(result.status, ControlApiCommandStatus::Executed);

        let dispatcher = api.into_dispatcher();
        assert_eq!(dispatcher.commands.len(), 1);
        match &dispatcher.commands[0] {
            RuntimeCommand::StrategyIntent(request) => {
                assert_eq!(request.action_source, ActionSource::System);
            }
            other => panic!("unexpected runtime command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn override_required_outcome_maps_to_control_api_status() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Ok(sample_execution_outcome(
                RiskDecisionStatus::RequiresOverride,
                false,
            ))),
        };
        let mut api = LocalControlApi::new(dispatcher);

        let result = api
            .handle_command(ControlApiCommand::ManualIntent {
                source: ManualCommandSource::Cli,
                request: sample_request(),
            })
            .await
            .expect("override-required outcome should still succeed");

        assert_eq!(result.status, ControlApiCommandStatus::RequiresOverride);
        assert_eq!(result.risk_status, RiskDecisionStatus::RequiresOverride);
        assert!(!result.dispatch_performed);
        assert_eq!(result.reason, "override required");
    }

    #[tokio::test]
    async fn http_handler_maps_manual_command_and_publishes_websocket_event() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Ok(sample_execution_outcome(
                RiskDecisionStatus::Accepted,
                true,
            ))),
        };
        let api = LocalControlApi::new(dispatcher);
        let hub = WebSocketEventHub::new(8).expect("hub should build");
        let mut stream = hub.subscribe();
        let mut handler = HttpCommandHandler::with_publisher(api, hub.clone());

        let response = handler
            .handle_command(HttpCommandRequest {
                command: ControlApiCommand::ManualIntent {
                    source: ManualCommandSource::Dashboard,
                    request: sample_request(),
                },
            })
            .await
            .expect("http handler should succeed");

        assert_eq!(response.status_code, HttpStatusCode::Ok);
        match response.body {
            HttpResponseBody::CommandResult(result) => {
                assert!(result.dispatch_performed);
                assert_eq!(result.status, ControlApiCommandStatus::Executed);
            }
            other => panic!("unexpected http response body: {other:?}"),
        }

        let event = stream.recv().await.expect("event should be published");
        match event {
            ControlApiEvent::CommandResult { source, result, .. } => {
                assert_eq!(source, ActionSource::Dashboard);
                assert_eq!(result.status, ControlApiCommandStatus::Executed);
            }
            other => panic!("unexpected websocket event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_handler_maps_override_required_to_precondition_required() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Ok(sample_execution_outcome(
                RiskDecisionStatus::RequiresOverride,
                false,
            ))),
        };
        let api = LocalControlApi::new(dispatcher);
        let mut handler = HttpCommandHandler::new(api);

        let response = handler
            .handle_command(HttpCommandRequest {
                command: ControlApiCommand::ManualIntent {
                    source: ManualCommandSource::Cli,
                    request: sample_request(),
                },
            })
            .await
            .expect("http handler should succeed");

        assert_eq!(response.status_code, HttpStatusCode::PreconditionRequired);
        match response.body {
            HttpResponseBody::CommandResult(result) => {
                assert_eq!(result.status, ControlApiCommandStatus::RequiresOverride);
                assert!(!result.dispatch_performed);
            }
            other => panic!("unexpected http response body: {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_handler_maps_runtime_errors_to_internal_server_error() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Err(RuntimeCommandError::Journal {
                source: tv_bot_journal::JournalError::Poisoned,
            })),
        };
        let api = LocalControlApi::new(dispatcher);
        let mut handler = HttpCommandHandler::new(api);

        let response = handler
            .handle_command(HttpCommandRequest {
                command: ControlApiCommand::StrategyIntent {
                    request: sample_request(),
                },
            })
            .await
            .expect("http handler should convert runtime errors into responses");

        assert_eq!(response.status_code, HttpStatusCode::InternalServerError);
        match response.body {
            HttpResponseBody::Error { message } => {
                assert!(message.contains("runtime command failed"));
            }
            other => panic!("unexpected http response body: {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_handler_maps_execution_planning_conflicts_to_conflict() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Err(RuntimeCommandError::Execution {
                source: RuntimeExecutionError::Dispatch {
                    source: ExecutionDispatchError::Planning {
                        source: ExecutionEngineError::NewEntriesBlocked,
                    },
                },
            })),
        };
        let api = LocalControlApi::new(dispatcher);
        let mut handler = HttpCommandHandler::new(api);

        let response = handler
            .handle_command(HttpCommandRequest {
                command: ControlApiCommand::StrategyIntent {
                    request: sample_request(),
                },
            })
            .await
            .expect("http handler should convert planning conflicts into responses");

        assert_eq!(response.status_code, HttpStatusCode::Conflict);
        match response.body {
            HttpResponseBody::Error { message } => {
                assert!(message.contains("new entries are blocked"));
            }
            other => panic!("unexpected http response body: {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_handler_maps_order_arming_preconditions_to_precondition_required() {
        let dispatcher = FakeDispatcher {
            commands: Vec::new(),
            next_result: Some(Err(RuntimeCommandError::Execution {
                source: RuntimeExecutionError::Dispatch {
                    source: ExecutionDispatchError::Planning {
                        source: ExecutionEngineError::OrderPlacementBlocked,
                    },
                },
            })),
        };
        let api = LocalControlApi::new(dispatcher);
        let mut handler = HttpCommandHandler::new(api);

        let response = handler
            .handle_command(HttpCommandRequest {
                command: ControlApiCommand::ManualIntent {
                    source: ManualCommandSource::Cli,
                    request: sample_request(),
                },
            })
            .await
            .expect("http handler should convert planning preconditions into responses");

        assert_eq!(response.status_code, HttpStatusCode::PreconditionRequired);
        match response.body {
            HttpResponseBody::Error { message } => {
                assert!(message.contains("runtime command failed"));
            }
            other => panic!("unexpected http response body: {other:?}"),
        }
    }

    #[tokio::test]
    async fn websocket_event_hub_broadcasts_events_to_subscribers() {
        let hub = WebSocketEventHub::new(4).expect("hub should build");
        let mut stream = hub.subscribe();

        hub.publish(ControlApiEvent::JournalRecord {
            record: tv_bot_core_types::EventJournalRecord {
                event_id: "evt-1".to_owned(),
                category: "manual".to_owned(),
                action: "clicked".to_owned(),
                source: ActionSource::Dashboard,
                severity: tv_bot_core_types::EventSeverity::Info,
                occurred_at: chrono::Utc::now(),
                payload: serde_json::json!({ "button": "flatten" }),
            },
        })
        .expect("publish should succeed");

        let event = stream.recv().await.expect("stream should receive event");
        match event {
            ControlApiEvent::JournalRecord { record } => {
                assert_eq!(record.action, "clicked");
                assert_eq!(record.category, "manual");
            }
            other => panic!("unexpected websocket event: {other:?}"),
        }
    }
}
