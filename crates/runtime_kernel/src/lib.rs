//! Runtime mode and readiness state management.

mod service_loop;

use chrono::Utc;
use serde_json::json;
use thiserror::Error;
use tv_bot_broker_tradovate::{
    Clock as TradovateClock, TradovateAccountApi, TradovateAuthApi, TradovateExecutionApi,
    TradovateSessionManager, TradovateSyncApi,
};
use tv_bot_core_types::{
    ActionSource, ActiveRuntimeMode, ArmReadinessReport, ArmState, BrokerAccountRouting,
    BrokerHealth, BrokerStatusSnapshot, BrokerSyncState, EventJournalRecord, EventSeverity,
    ReadinessCheck, ReadinessCheckStatus, RiskDecisionStatus, RuntimeMode, WarmupStatus,
};
use tv_bot_execution_engine::{
    plan_and_execute_tradovate, ExecutionDispatchError, ExecutionDispatchReport, ExecutionRequest,
};
use tv_bot_journal::{EventJournal, JournalError};
use tv_bot_market_data::{MarketDataHealth, MarketDataStatusSnapshot, WarmupProgress};
use tv_bot_risk_engine::{
    RiskEvaluationOutcome, RiskEvaluationRequest, RiskEvaluator, RiskInstrumentContext,
    RiskStateContext,
};

pub use service_loop::{
    RuntimeCommand, RuntimeCommandError, RuntimeCommandOutcome, RuntimeControlLoop,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyHealth {
    Healthy,
    Warning(String),
    Blocking(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessInputs {
    pub mode: RuntimeMode,
    pub strategy_loaded: bool,
    pub warmup_status: WarmupStatus,
    pub account_selection: DependencyHealth,
    pub symbol_mapping_resolved: bool,
    pub market_data: DependencyHealth,
    pub broker_sync: DependencyHealth,
    pub storage: DependencyHealth,
    pub journal: DependencyHealth,
    pub clock: DependencyHealth,
    pub risk_summary: String,
    pub hard_override_reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStateMachine {
    current_mode: RuntimeMode,
    last_active_mode: Option<ActiveRuntimeMode>,
    arm_state: ArmState,
    warmup_status: WarmupStatus,
    strategy_loaded: bool,
    hard_override_active: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeKernelError {
    #[error("cannot resume because no prior active mode is available")]
    ResumeTargetUnknown,
    #[error("strategy must be loaded before this transition is allowed")]
    StrategyNotLoaded,
    #[error("warmup must be ready before arming or trading")]
    WarmupNotReady,
    #[error("cannot arm while mode is `{0:?}`")]
    CannotArmInMode(RuntimeMode),
    #[error("readiness report does not match the current mode")]
    ReportModeMismatch,
    #[error("readiness report contains blocking issues")]
    ReadinessBlocked,
    #[error("a hard override is required before arming")]
    OverrideRequired,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeExecutionRequest {
    pub mode: RuntimeMode,
    pub action_source: ActionSource,
    pub execution: ExecutionRequest,
    pub risk_instrument: RiskInstrumentContext,
    pub risk_state: RiskStateContext,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeExecutionOutcome {
    pub risk: RiskEvaluationOutcome,
    pub dispatch: Option<ExecutionDispatchReport>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeExecutionError {
    #[error("journal append failed: {source}")]
    Journal { source: JournalError },
    #[error("execution dispatch failed: {source}")]
    Dispatch { source: ExecutionDispatchError },
}

impl RuntimeStateMachine {
    pub fn new(startup_mode: RuntimeMode) -> Self {
        Self {
            current_mode: startup_mode.clone(),
            last_active_mode: active_mode_for(&startup_mode),
            arm_state: ArmState::Disarmed,
            warmup_status: WarmupStatus::NotLoaded,
            strategy_loaded: false,
            hard_override_active: false,
        }
    }

    pub fn current_mode(&self) -> RuntimeMode {
        self.current_mode.clone()
    }

    pub fn arm_state(&self) -> ArmState {
        self.arm_state.clone()
    }

    pub fn warmup_status(&self) -> WarmupStatus {
        self.warmup_status.clone()
    }

    pub fn is_strategy_loaded(&self) -> bool {
        self.strategy_loaded
    }

    pub fn hard_override_active(&self) -> bool {
        self.hard_override_active
    }

    pub fn switch_mode(&mut self, mode: RuntimeMode) {
        if self.current_mode != mode {
            if matches!(
                mode,
                RuntimeMode::Paper | RuntimeMode::Live | RuntimeMode::Observation
            ) {
                self.arm_state = ArmState::Disarmed;
                self.hard_override_active = false;
            }
        }

        if let Some(active_mode) = active_mode_for(&mode) {
            self.last_active_mode = Some(active_mode);
        }

        if matches!(mode, RuntimeMode::Observation) {
            self.arm_state = ArmState::Disarmed;
            self.hard_override_active = false;
        }

        self.current_mode = mode;
    }

    pub fn pause(&mut self) {
        if let Some(active_mode) = active_mode_for(&self.current_mode) {
            self.last_active_mode = Some(active_mode);
        }
        self.current_mode = RuntimeMode::Paused;
    }

    pub fn resume(&mut self) -> Result<(), RuntimeKernelError> {
        let mode = self
            .last_active_mode
            .clone()
            .ok_or(RuntimeKernelError::ResumeTargetUnknown)?;
        self.current_mode = match mode {
            ActiveRuntimeMode::Paper => RuntimeMode::Paper,
            ActiveRuntimeMode::Live => RuntimeMode::Live,
            ActiveRuntimeMode::Observation => RuntimeMode::Observation,
        };
        Ok(())
    }

    pub fn mark_strategy_loaded(&mut self) {
        self.strategy_loaded = true;
        self.warmup_status = WarmupStatus::Loaded;
        self.arm_state = ArmState::Disarmed;
        self.hard_override_active = false;
    }

    pub fn unload_strategy(&mut self) {
        self.strategy_loaded = false;
        self.warmup_status = WarmupStatus::NotLoaded;
        self.arm_state = ArmState::Disarmed;
        self.hard_override_active = false;
    }

    pub fn start_warmup(&mut self) -> Result<(), RuntimeKernelError> {
        if !self.strategy_loaded {
            return Err(RuntimeKernelError::StrategyNotLoaded);
        }

        self.warmup_status = WarmupStatus::Warming;
        Ok(())
    }

    pub fn mark_warmup_ready(&mut self) -> Result<(), RuntimeKernelError> {
        if !self.strategy_loaded {
            return Err(RuntimeKernelError::StrategyNotLoaded);
        }

        self.warmup_status = WarmupStatus::Ready;
        Ok(())
    }

    pub fn mark_warmup_failed(&mut self) -> Result<(), RuntimeKernelError> {
        if !self.strategy_loaded {
            return Err(RuntimeKernelError::StrategyNotLoaded);
        }

        self.warmup_status = WarmupStatus::Failed;
        self.arm_state = ArmState::Disarmed;
        self.hard_override_active = false;
        Ok(())
    }

    pub fn sync_warmup_progress(
        &mut self,
        progress: &WarmupProgress,
    ) -> Result<(), RuntimeKernelError> {
        if !self.strategy_loaded {
            return Err(RuntimeKernelError::StrategyNotLoaded);
        }

        self.warmup_status = progress.status.clone();

        if matches!(progress.status, WarmupStatus::Failed) {
            self.arm_state = ArmState::Disarmed;
            self.hard_override_active = false;
        }

        Ok(())
    }

    pub fn arm(
        &mut self,
        readiness: &ArmReadinessReport,
        allow_override: bool,
    ) -> Result<(), RuntimeKernelError> {
        match self.current_mode {
            RuntimeMode::Paper | RuntimeMode::Live => {}
            _ => {
                return Err(RuntimeKernelError::CannotArmInMode(
                    self.current_mode.clone(),
                ))
            }
        }

        if readiness.mode != self.current_mode {
            return Err(RuntimeKernelError::ReportModeMismatch);
        }

        if !self.strategy_loaded {
            return Err(RuntimeKernelError::StrategyNotLoaded);
        }

        if self.warmup_status != WarmupStatus::Ready {
            return Err(RuntimeKernelError::WarmupNotReady);
        }

        if readiness.has_blocking_issues() {
            return Err(RuntimeKernelError::ReadinessBlocked);
        }

        if readiness.hard_override_required && !allow_override {
            return Err(RuntimeKernelError::OverrideRequired);
        }

        self.arm_state = ArmState::Armed;
        self.hard_override_active = readiness.hard_override_required && allow_override;
        Ok(())
    }

    pub fn disarm(&mut self) {
        self.arm_state = ArmState::Disarmed;
        self.hard_override_active = false;
    }

    pub fn can_submit_orders(&self) -> bool {
        matches!(self.current_mode, RuntimeMode::Paper | RuntimeMode::Live)
            && self.arm_state == ArmState::Armed
            && self.strategy_loaded
            && self.warmup_status == WarmupStatus::Ready
    }
}

pub struct ReadinessEvaluator;

impl ReadinessEvaluator {
    pub fn evaluate(input: ReadinessInputs) -> ArmReadinessReport {
        let mut checks = vec![
            readiness_check(
                "mode",
                match input.mode {
                    RuntimeMode::Paper | RuntimeMode::Live => DependencyHealth::Healthy,
                    RuntimeMode::Observation => DependencyHealth::Blocking(
                        "observation mode cannot be armed for trading".to_owned(),
                    ),
                    RuntimeMode::Paused => DependencyHealth::Blocking(
                        "paused mode must be resumed into paper or live before arming".to_owned(),
                    ),
                },
            ),
            readiness_check(
                "strategy_loaded",
                if input.strategy_loaded {
                    DependencyHealth::Healthy
                } else {
                    DependencyHealth::Blocking("no strategy is currently loaded".to_owned())
                },
            ),
            readiness_check(
                "warmup",
                match input.warmup_status {
                    WarmupStatus::Ready => DependencyHealth::Healthy,
                    WarmupStatus::NotLoaded => {
                        DependencyHealth::Blocking("warmup has not been prepared".to_owned())
                    }
                    WarmupStatus::Loaded => DependencyHealth::Blocking(
                        "strategy is loaded but warmup has not started".to_owned(),
                    ),
                    WarmupStatus::Warming => {
                        DependencyHealth::Blocking("warmup is still in progress".to_owned())
                    }
                    WarmupStatus::Failed => {
                        DependencyHealth::Blocking("warmup failed and needs review".to_owned())
                    }
                },
            ),
            readiness_check("account_selected", input.account_selection),
            readiness_check(
                "symbol_mapping",
                if input.symbol_mapping_resolved {
                    DependencyHealth::Healthy
                } else {
                    DependencyHealth::Blocking(
                        "strategy market intent has not been resolved to a broker symbol"
                            .to_owned(),
                    )
                },
            ),
            readiness_check("market_data", input.market_data),
            readiness_check("broker_sync", input.broker_sync),
            readiness_check("storage", input.storage),
            readiness_check("journal", input.journal),
            readiness_check("clock", input.clock),
            readiness_check(
                "risk_summary",
                if input.risk_summary.trim().is_empty() {
                    DependencyHealth::Blocking("risk summary must be present".to_owned())
                } else {
                    DependencyHealth::Healthy
                },
            ),
        ];

        for (index, reason) in input.hard_override_reasons.iter().enumerate() {
            checks.push(ReadinessCheck {
                name: format!("override_requirement_{index}"),
                status: ReadinessCheckStatus::Warning,
                message: reason.clone(),
            });
        }

        ArmReadinessReport {
            mode: input.mode,
            checks,
            risk_summary: input.risk_summary,
            hard_override_required: !input.hard_override_reasons.is_empty(),
            generated_at: Utc::now(),
        }
    }

    pub fn market_data_dependency(snapshot: &MarketDataStatusSnapshot) -> DependencyHealth {
        match snapshot.health {
            MarketDataHealth::Healthy => DependencyHealth::Healthy,
            MarketDataHealth::Initializing => DependencyHealth::Blocking(
                "market data subscriptions are connected but required feeds are not ready yet"
                    .to_owned(),
            ),
            MarketDataHealth::Degraded => DependencyHealth::Blocking(
                "market data is degraded; new entries must remain blocked".to_owned(),
            ),
            MarketDataHealth::Disconnected => {
                DependencyHealth::Blocking("market data is disconnected".to_owned())
            }
            MarketDataHealth::Failed => {
                DependencyHealth::Blocking("market data adapter reported a failure".to_owned())
            }
        }
    }

    pub fn broker_account_dependency(
        mode: &RuntimeMode,
        snapshot: &BrokerStatusSnapshot,
    ) -> DependencyHealth {
        let Some(selected_account) = &snapshot.selected_account else {
            return DependencyHealth::Blocking("no broker account is selected".to_owned());
        };

        match (mode, selected_account.routing) {
            (RuntimeMode::Paper, BrokerAccountRouting::Paper)
            | (RuntimeMode::Live, BrokerAccountRouting::Live) => DependencyHealth::Healthy,
            (RuntimeMode::Paper, BrokerAccountRouting::Live) => DependencyHealth::Blocking(
                "selected broker account is routed for live trading while runtime mode is paper"
                    .to_owned(),
            ),
            (RuntimeMode::Live, BrokerAccountRouting::Paper) => DependencyHealth::Blocking(
                "selected broker account is routed for paper trading while runtime mode is live"
                    .to_owned(),
            ),
            (RuntimeMode::Observation, _) | (RuntimeMode::Paused, _) => DependencyHealth::Healthy,
        }
    }

    pub fn broker_sync_dependency(snapshot: &BrokerStatusSnapshot) -> DependencyHealth {
        match snapshot.sync_state {
            BrokerSyncState::Synchronized if snapshot.health == BrokerHealth::Healthy => {
                DependencyHealth::Healthy
            }
            BrokerSyncState::Pending | BrokerSyncState::Synchronized
                if snapshot.health == BrokerHealth::Initializing =>
            {
                DependencyHealth::Blocking(
                    "broker session is connected but account sync is still initializing".to_owned(),
                )
            }
            BrokerSyncState::Stale => DependencyHealth::Blocking(
                "broker sync is stale; new entries must remain blocked".to_owned(),
            ),
            BrokerSyncState::Mismatch => {
                DependencyHealth::Blocking(snapshot.review_required_reason.clone().unwrap_or_else(
                    || "broker sync mismatch is active; new entries must remain blocked".to_owned(),
                ))
            }
            BrokerSyncState::ReviewRequired => {
                DependencyHealth::Blocking(snapshot.review_required_reason.clone().unwrap_or_else(
                    || "broker reconnect requires manual review before arming".to_owned(),
                ))
            }
            BrokerSyncState::Disconnected
                if matches!(
                    snapshot.health,
                    BrokerHealth::Disconnected | BrokerHealth::Degraded
                ) =>
            {
                DependencyHealth::Blocking(
                    "broker session is disconnected; new entries must remain blocked".to_owned(),
                )
            }
            BrokerSyncState::Failed | BrokerSyncState::Disconnected
                if snapshot.health == BrokerHealth::Failed =>
            {
                DependencyHealth::Blocking("broker adapter reported a failure".to_owned())
            }
            _ if snapshot.health == BrokerHealth::Healthy => DependencyHealth::Healthy,
            _ if snapshot.health == BrokerHealth::Degraded => DependencyHealth::Blocking(
                "broker health is degraded; new entries must remain blocked".to_owned(),
            ),
            _ if snapshot.health == BrokerHealth::Disconnected => {
                DependencyHealth::Blocking("broker session is disconnected".to_owned())
            }
            _ => DependencyHealth::Blocking("broker adapter reported a failure".to_owned()),
        }
    }
}

pub async fn evaluate_risk_and_execute_tradovate<A, B, C, Clk, E, J>(
    request: &RuntimeExecutionRequest,
    session: &mut TradovateSessionManager<A, B, C, Clk>,
    execution_api: &E,
    journal: &J,
) -> Result<RuntimeExecutionOutcome, RuntimeExecutionError>
where
    A: TradovateAuthApi,
    B: TradovateAccountApi,
    C: TradovateSyncApi,
    Clk: TradovateClock,
    E: TradovateExecutionApi,
    J: EventJournal,
{
    let risk = RiskEvaluator::evaluate(&RiskEvaluationRequest {
        strategy: request.execution.strategy.clone(),
        instrument: request.risk_instrument.clone(),
        state: request.risk_state.clone(),
        intent: request.execution.intent.clone(),
    });

    journal_risk_decision(request, &risk, journal)
        .map_err(|source| RuntimeExecutionError::Journal { source })?;

    if !risk.hard_override_reasons.is_empty() {
        journal_hard_override(request, &risk, journal)
            .map_err(|source| RuntimeExecutionError::Journal { source })?;
    }

    if risk.decision.status != RiskDecisionStatus::Accepted {
        return Ok(RuntimeExecutionOutcome {
            risk,
            dispatch: None,
        });
    }

    let mut execution = request.execution.clone();
    execution.intent = risk.adjusted_intent.clone();

    let dispatch = plan_and_execute_tradovate(&execution, session, execution_api)
        .await
        .map_err(|source| RuntimeExecutionError::Dispatch { source })?;

    Ok(RuntimeExecutionOutcome {
        risk,
        dispatch: Some(dispatch),
    })
}

fn readiness_check(name: &str, dependency: DependencyHealth) -> ReadinessCheck {
    match dependency {
        DependencyHealth::Healthy => ReadinessCheck {
            name: name.to_owned(),
            status: ReadinessCheckStatus::Pass,
            message: "ok".to_owned(),
        },
        DependencyHealth::Warning(message) => ReadinessCheck {
            name: name.to_owned(),
            status: ReadinessCheckStatus::Warning,
            message,
        },
        DependencyHealth::Blocking(message) => ReadinessCheck {
            name: name.to_owned(),
            status: ReadinessCheckStatus::Blocking,
            message,
        },
    }
}

fn active_mode_for(mode: &RuntimeMode) -> Option<ActiveRuntimeMode> {
    match mode {
        RuntimeMode::Paper => Some(ActiveRuntimeMode::Paper),
        RuntimeMode::Live => Some(ActiveRuntimeMode::Live),
        RuntimeMode::Observation => Some(ActiveRuntimeMode::Observation),
        RuntimeMode::Paused => None,
    }
}

fn journal_risk_decision<J: EventJournal>(
    request: &RuntimeExecutionRequest,
    outcome: &RiskEvaluationOutcome,
    journal: &J,
) -> Result<(), JournalError> {
    let occurred_at = Utc::now();
    journal.append(EventJournalRecord {
        event_id: event_id("risk", "decision", occurred_at),
        category: "risk".to_owned(),
        action: "decision".to_owned(),
        source: request.action_source,
        severity: match outcome.decision.status {
            RiskDecisionStatus::Accepted => EventSeverity::Info,
            RiskDecisionStatus::Rejected | RiskDecisionStatus::RequiresOverride => {
                EventSeverity::Warning
            }
        },
        occurred_at,
        payload: json!({
            "mode": request.mode,
            "strategy_id": request.execution.strategy.metadata.strategy_id,
            "intent": intent_name(&request.execution.intent),
            "decision_status": outcome.decision.status,
            "reason": outcome.decision.reason,
            "warnings": outcome.decision.warnings,
            "approved_quantity": outcome.approved_quantity,
            "hard_override_reasons": outcome.hard_override_reasons,
        }),
    })
}

fn journal_hard_override<J: EventJournal>(
    request: &RuntimeExecutionRequest,
    outcome: &RiskEvaluationOutcome,
    journal: &J,
) -> Result<(), JournalError> {
    let occurred_at = Utc::now();
    let action = if request.risk_state.hard_override_active
        && outcome.decision.status == RiskDecisionStatus::Accepted
    {
        "hard_override_used"
    } else {
        "hard_override_required"
    };

    journal.append(EventJournalRecord {
        event_id: event_id("risk", action, occurred_at),
        category: "risk".to_owned(),
        action: action.to_owned(),
        source: request.action_source,
        severity: EventSeverity::Warning,
        occurred_at,
        payload: json!({
            "mode": request.mode,
            "strategy_id": request.execution.strategy.metadata.strategy_id,
            "intent": intent_name(&request.execution.intent),
            "decision_status": outcome.decision.status,
            "reasons": outcome.hard_override_reasons,
        }),
    })
}

fn intent_name(intent: &tv_bot_core_types::ExecutionIntent) -> &'static str {
    match intent {
        tv_bot_core_types::ExecutionIntent::Enter { .. } => "enter",
        tv_bot_core_types::ExecutionIntent::Exit { .. } => "exit",
        tv_bot_core_types::ExecutionIntent::Flatten { .. } => "flatten",
        tv_bot_core_types::ExecutionIntent::CancelWorkingOrders { .. } => "cancel_working_orders",
        tv_bot_core_types::ExecutionIntent::ReducePosition { .. } => "reduce_position",
        tv_bot_core_types::ExecutionIntent::PauseStrategy { .. } => "pause_strategy",
    }
}

fn event_id(category: &str, action: &str, occurred_at: chrono::DateTime<chrono::Utc>) -> String {
    let timestamp = occurred_at.timestamp_nanos_opt().unwrap_or_default();
    format!("{category}-{action}-{timestamp}")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use rust_decimal::Decimal;
    use secrecy::SecretString;
    use tv_bot_broker_tradovate::{
        TradovateAccessToken, TradovateAccount, TradovateAccountApi, TradovateAccountListRequest,
        TradovateAuthApi, TradovateAuthRequest, TradovateCredentials, TradovateError,
        TradovateExecutionApi, TradovateLiquidatePositionRequest, TradovateLiquidatePositionResult,
        TradovateOrderType, TradovatePlaceOrderRequest, TradovatePlaceOrderResult,
        TradovatePlaceOsoRequest, TradovatePlaceOsoResult, TradovateRoutingPreferences,
        TradovateSessionConfig, TradovateSessionManager, TradovateSyncApi,
        TradovateSyncConnectRequest, TradovateSyncEvent, TradovateSyncSnapshot,
        TradovateTimeInForce, TradovateUserSyncRequest,
    };
    use tv_bot_core_types::{
        ActionSource, BreakEvenRule, BrokerEnvironment, BrokerOrderUpdate, BrokerPreference,
        CompiledStrategy, ContractMode, DailyLossLimit, DashboardDisplay, DataFeedRequirement,
        DataRequirements, EntryOrderType, EntryRules, ExecutionIntent, ExecutionSpec, ExitRules,
        FailsafeRules, FeedType, MarketConfig, MarketSelection, PartialTakeProfitRule,
        PositionSizing, PositionSizingMode, ReversalMode, RiskLimits, ScalingConfig, SessionMode,
        SessionRules, SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata,
        Timeframe, TradeManagement, TradeSide, TrailingRule,
    };
    use tv_bot_execution_engine::{ExecutionInstrumentContext, ExecutionStateContext};
    use tv_bot_journal::{EventJournal, InMemoryJournal};

    use super::*;

    fn ready_report(mode: RuntimeMode) -> ArmReadinessReport {
        ReadinessEvaluator::evaluate(ReadinessInputs {
            mode,
            strategy_loaded: true,
            warmup_status: WarmupStatus::Ready,
            account_selection: DependencyHealth::Healthy,
            symbol_mapping_resolved: true,
            market_data: DependencyHealth::Healthy,
            broker_sync: DependencyHealth::Healthy,
            storage: DependencyHealth::Healthy,
            journal: DependencyHealth::Healthy,
            clock: DependencyHealth::Healthy,
            risk_summary: "fixed contracts, broker-required stop/tp".to_owned(),
            hard_override_reasons: Vec::new(),
        })
    }

    #[derive(Clone)]
    struct FakeAuthApi {
        token: Arc<Mutex<Option<TradovateAccessToken>>>,
    }

    #[async_trait]
    impl TradovateAuthApi for FakeAuthApi {
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
                environment: BrokerEnvironment::Demo,
                credentials: sample_credentials(),
            })
            .await
        }
    }

    #[derive(Clone)]
    struct FakeAccountApi {
        accounts: Arc<Vec<TradovateAccount>>,
    }

    #[async_trait]
    impl TradovateAccountApi for FakeAccountApi {
        async fn list_accounts(
            &self,
            _request: TradovateAccountListRequest,
        ) -> Result<Vec<TradovateAccount>, TradovateError> {
            Ok(self.accounts.as_ref().clone())
        }
    }

    #[derive(Clone)]
    struct FakeSyncApi {
        snapshots: Arc<Mutex<VecDeque<TradovateSyncSnapshot>>>,
    }

    #[async_trait]
    impl TradovateSyncApi for FakeSyncApi {
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

    #[derive(Clone, Debug, Default)]
    struct FakeExecutionApi {
        place_orders: Arc<Mutex<Vec<TradovatePlaceOrderRequest>>>,
        place_osos: Arc<Mutex<Vec<TradovatePlaceOsoRequest>>>,
        liquidations: Arc<Mutex<Vec<TradovateLiquidatePositionRequest>>>,
    }

    #[async_trait]
    impl TradovateExecutionApi for FakeExecutionApi {
        async fn place_order(
            &self,
            request: TradovatePlaceOrderRequest,
        ) -> Result<TradovatePlaceOrderResult, TradovateError> {
            self.place_orders
                .lock()
                .expect("execution mutex should not poison")
                .push(request);
            Ok(TradovatePlaceOrderResult { order_id: 7101 })
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
                order_id: 7102,
                oso1_id: Some(7103),
                oso2_id: Some(7104),
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
            Ok(TradovateLiquidatePositionResult { order_id: 7105 })
        }
    }

    fn sample_strategy() -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "gc_runtime_risk_v1".to_owned(),
                name: "GC Runtime Risk".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "runtime orchestration tests".to_owned(),
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
                contracts: Some(2),
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

    fn sample_execution_request(strategy: CompiledStrategy) -> ExecutionRequest {
        ExecutionRequest {
            strategy,
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
                working_orders: Vec::<BrokerOrderUpdate>::new(),
            },
            intent: ExecutionIntent::Enter {
                side: TradeSide::Buy,
                order_type: EntryOrderType::Market,
                quantity: 1,
                protective_brackets_expected: false,
                reason: "runtime entry".to_owned(),
            },
        }
    }

    fn sample_risk_state() -> RiskStateContext {
        RiskStateContext {
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
        }
    }

    fn sample_runtime_execution_request(strategy: CompiledStrategy) -> RuntimeExecutionRequest {
        RuntimeExecutionRequest {
            mode: RuntimeMode::Paper,
            action_source: ActionSource::System,
            execution: sample_execution_request(strategy),
            risk_instrument: RiskInstrumentContext {
                tick_value_usd: Some(Decimal::new(10, 0)),
            },
            risk_state: sample_risk_state(),
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
            expiration_time: DateTime::parse_from_rfc3339("2026-04-10T15:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            issued_at: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            user_id: Some(7),
            person_id: Some(11),
            market_data_access: Some("realtime".to_owned()),
        }
    }

    fn empty_sync_snapshot() -> TradovateSyncSnapshot {
        TradovateSyncSnapshot {
            occurred_at: DateTime::parse_from_rfc3339("2026-04-10T13:30:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            positions: Vec::new(),
            working_orders: Vec::new(),
            fills: Vec::new(),
            account_snapshot: None,
            mismatch_reason: None,
            detail: "synced".to_owned(),
        }
    }

    async fn sample_session_manager(
    ) -> TradovateSessionManager<FakeAuthApi, FakeAccountApi, FakeSyncApi> {
        let auth_api = FakeAuthApi {
            token: Arc::new(Mutex::new(Some(sample_token()))),
        };
        let account_api = FakeAccountApi {
            accounts: Arc::new(vec![TradovateAccount {
                account_id: 101,
                account_name: "paper-primary".to_owned(),
                nickname: None,
                active: true,
            }]),
        };
        let sync_api = FakeSyncApi {
            snapshots: Arc::new(Mutex::new(VecDeque::from([empty_sync_snapshot()]))),
        };

        let mut manager = TradovateSessionManager::with_system_clock(
            TradovateSessionConfig::new(
                BrokerEnvironment::Demo,
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
            .expect("account selection should succeed");
        manager
            .connect_user_sync()
            .await
            .expect("sync should connect");

        manager
    }

    #[test]
    fn pause_and_resume_preserve_last_active_mode() {
        let mut state = RuntimeStateMachine::new(RuntimeMode::Paper);
        state.pause();
        assert_eq!(state.current_mode(), RuntimeMode::Paused);

        state.resume().expect("resume should succeed");
        assert_eq!(state.current_mode(), RuntimeMode::Paper);
    }

    #[test]
    fn arming_requires_ready_strategy_and_trade_mode() {
        let mut state = RuntimeStateMachine::new(RuntimeMode::Observation);
        state.mark_strategy_loaded();
        state
            .mark_warmup_ready()
            .expect("warmup should become ready");

        let error = state
            .arm(&ready_report(RuntimeMode::Observation), false)
            .expect_err("observation cannot be armed");

        assert_eq!(
            error,
            RuntimeKernelError::CannotArmInMode(RuntimeMode::Observation)
        );

        state.switch_mode(RuntimeMode::Paper);
        state
            .arm(&ready_report(RuntimeMode::Paper), false)
            .expect("paper mode with ready state should allow arming");
        assert_eq!(state.arm_state(), ArmState::Armed);
    }

    #[test]
    fn hard_override_must_be_explicit() {
        let mut state = RuntimeStateMachine::new(RuntimeMode::Live);
        state.mark_strategy_loaded();
        state
            .mark_warmup_ready()
            .expect("warmup should become ready");

        let report = ReadinessEvaluator::evaluate(ReadinessInputs {
            mode: RuntimeMode::Live,
            strategy_loaded: true,
            warmup_status: WarmupStatus::Ready,
            account_selection: DependencyHealth::Healthy,
            symbol_mapping_resolved: true,
            market_data: DependencyHealth::Healthy,
            broker_sync: DependencyHealth::Healthy,
            storage: DependencyHealth::Warning(
                "primary Postgres is unavailable, SQLite fallback would require override"
                    .to_owned(),
            ),
            journal: DependencyHealth::Healthy,
            clock: DependencyHealth::Healthy,
            risk_summary: "risk-based sizing, broker-required stop/tp".to_owned(),
            hard_override_reasons: vec!["storage is degraded".to_owned()],
        });

        assert!(report.hard_override_required);
        assert_eq!(
            state.arm(&report, false),
            Err(RuntimeKernelError::OverrideRequired)
        );

        state
            .arm(&report, true)
            .expect("override path should allow arming");
        assert_eq!(state.arm_state(), ArmState::Armed);
        assert!(state.hard_override_active());
    }

    #[test]
    fn readiness_evaluator_blocks_when_not_ready() {
        let report = ReadinessEvaluator::evaluate(ReadinessInputs {
            mode: RuntimeMode::Paused,
            strategy_loaded: false,
            warmup_status: WarmupStatus::Loaded,
            account_selection: DependencyHealth::Blocking(
                "no broker account is selected".to_owned(),
            ),
            symbol_mapping_resolved: false,
            market_data: DependencyHealth::Blocking("market data disconnected".to_owned()),
            broker_sync: DependencyHealth::Healthy,
            storage: DependencyHealth::Healthy,
            journal: DependencyHealth::Healthy,
            clock: DependencyHealth::Healthy,
            risk_summary: String::new(),
            hard_override_reasons: Vec::new(),
        });

        assert!(report.has_blocking_issues());
        assert!(!report.is_ready_without_override());
    }

    #[test]
    fn can_submit_orders_only_when_armed_ready_and_unpaused() {
        let mut state = RuntimeStateMachine::new(RuntimeMode::Paper);
        state.mark_strategy_loaded();
        state.start_warmup().expect("warmup should start");
        state.mark_warmup_ready().expect("warmup should finish");
        state
            .arm(&ready_report(RuntimeMode::Paper), false)
            .expect("arming should succeed");
        assert!(state.can_submit_orders());

        state.pause();
        assert!(!state.can_submit_orders());
    }

    #[test]
    fn market_data_snapshot_maps_to_readiness_dependency() {
        let snapshot = MarketDataStatusSnapshot {
            provider: "databento",
            dataset: "GLBX.MDP3".to_owned(),
            connection_state: tv_bot_market_data::MarketDataConnectionState::Subscribed,
            health: MarketDataHealth::Degraded,
            feed_statuses: Vec::new(),
            warmup: WarmupProgress {
                status: WarmupStatus::Warming,
                ready_requires_all: true,
                buffers: Vec::new(),
                started_at: None,
                updated_at: Utc::now(),
                failure_reason: None,
            },
            reconnect_count: 0,
            last_heartbeat_at: None,
            last_disconnect_reason: None,
            updated_at: Utc::now(),
        };

        assert_eq!(
            ReadinessEvaluator::market_data_dependency(&snapshot),
            DependencyHealth::Blocking(
                "market data is degraded; new entries must remain blocked".to_owned()
            )
        );
    }

    #[test]
    fn syncing_failed_warmup_disarms_runtime() {
        let mut state = RuntimeStateMachine::new(RuntimeMode::Paper);
        state.mark_strategy_loaded();
        state
            .mark_warmup_ready()
            .expect("warmup should become ready");
        state
            .arm(&ready_report(RuntimeMode::Paper), false)
            .expect("arming should succeed");

        let progress = WarmupProgress {
            status: WarmupStatus::Failed,
            ready_requires_all: true,
            buffers: Vec::new(),
            started_at: Some(Utc::now()),
            updated_at: Utc::now(),
            failure_reason: Some("feed gap".to_owned()),
        };

        state
            .sync_warmup_progress(&progress)
            .expect("sync should succeed");

        assert_eq!(state.arm_state(), ArmState::Disarmed);
        assert_eq!(state.warmup_status(), WarmupStatus::Failed);
    }

    #[test]
    fn broker_account_dependency_blocks_route_mismatch() {
        let snapshot = BrokerStatusSnapshot {
            provider: "tradovate".to_owned(),
            environment: tv_bot_core_types::BrokerEnvironment::Demo,
            connection_state: tv_bot_core_types::BrokerConnectionState::Connected,
            health: BrokerHealth::Healthy,
            sync_state: BrokerSyncState::Synchronized,
            selected_account: Some(tv_bot_core_types::BrokerAccountSelection {
                provider: "tradovate".to_owned(),
                account_id: "42".to_owned(),
                account_name: "primary-live".to_owned(),
                routing: BrokerAccountRouting::Live,
                environment: tv_bot_core_types::BrokerEnvironment::Demo,
                selected_at: Utc::now(),
            }),
            reconnect_count: 0,
            last_authenticated_at: Some(Utc::now()),
            last_heartbeat_at: Some(Utc::now()),
            last_sync_at: Some(Utc::now()),
            last_disconnect_reason: None,
            review_required_reason: None,
            updated_at: Utc::now(),
        };

        assert_eq!(
            ReadinessEvaluator::broker_account_dependency(&RuntimeMode::Paper, &snapshot),
            DependencyHealth::Blocking(
                "selected broker account is routed for live trading while runtime mode is paper"
                    .to_owned()
            )
        );
    }

    #[test]
    fn broker_sync_dependency_blocks_reconnect_review() {
        let snapshot = BrokerStatusSnapshot {
            provider: "tradovate".to_owned(),
            environment: tv_bot_core_types::BrokerEnvironment::Live,
            connection_state: tv_bot_core_types::BrokerConnectionState::Connected,
            health: BrokerHealth::Degraded,
            sync_state: BrokerSyncState::ReviewRequired,
            selected_account: None,
            reconnect_count: 1,
            last_authenticated_at: Some(Utc::now()),
            last_heartbeat_at: Some(Utc::now()),
            last_sync_at: Some(Utc::now()),
            last_disconnect_reason: Some("socket reset".to_owned()),
            review_required_reason: Some(
                "existing broker-side position detected after reconnect".to_owned(),
            ),
            updated_at: Utc::now(),
        };

        assert_eq!(
            ReadinessEvaluator::broker_sync_dependency(&snapshot),
            DependencyHealth::Blocking(
                "existing broker-side position detected after reconnect".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn runtime_orchestration_applies_risk_sizing_before_dispatch() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let outcome = evaluate_risk_and_execute_tradovate(
            &sample_runtime_execution_request(sample_strategy()),
            &mut manager,
            &execution_api,
            &journal,
        )
        .await
        .expect("runtime orchestration should succeed");

        assert_eq!(outcome.risk.decision.status, RiskDecisionStatus::Accepted);
        assert_eq!(outcome.risk.approved_quantity, Some(2));
        assert!(outcome.dispatch.is_some());

        let orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].context.account_id, 101);
        assert_eq!(orders[0].order.quantity, 2);
        assert_eq!(orders[0].order.order_type, TradovateOrderType::Market);
        assert_eq!(
            orders[0].order.time_in_force,
            Some(TradovateTimeInForce::Day)
        );
        drop(orders);

        let journal_records = journal.list().expect("journal should list records");
        assert_eq!(journal_records.len(), 1);
        assert_eq!(journal_records[0].category, "risk");
        assert_eq!(journal_records[0].action, "decision");
        assert_eq!(
            journal_records[0].payload["approved_quantity"]
                .as_u64()
                .expect("approved quantity should be encoded"),
            2
        );
    }

    #[tokio::test]
    async fn rejected_risk_decision_is_journaled_and_blocks_dispatch() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let mut request = sample_runtime_execution_request(sample_strategy());
        request.risk_state.trades_today = request.execution.strategy.risk.max_trades_per_day;

        let outcome =
            evaluate_risk_and_execute_tradovate(&request, &mut manager, &execution_api, &journal)
                .await
                .expect("risk rejection should still complete cleanly");

        assert_eq!(outcome.risk.decision.status, RiskDecisionStatus::Rejected);
        assert!(outcome.dispatch.is_none());
        assert!(execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());

        let journal_records = journal.list().expect("journal should list records");
        assert_eq!(journal_records.len(), 1);
        assert_eq!(journal_records[0].action, "decision");
        assert_eq!(journal_records[0].severity, EventSeverity::Warning);
        assert_eq!(journal_records[0].payload["decision_status"], "rejected");
    }

    #[tokio::test]
    async fn override_requirement_is_journaled_and_blocks_dispatch() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let mut strategy = sample_strategy();
        strategy.execution.broker_preferences.stop_loss = BrokerPreference::BrokerRequired;

        let mut request = sample_runtime_execution_request(strategy);
        request.risk_state.broker_support.stop_loss = false;

        let outcome =
            evaluate_risk_and_execute_tradovate(&request, &mut manager, &execution_api, &journal)
                .await
                .expect("override requirement should be surfaced cleanly");

        assert_eq!(
            outcome.risk.decision.status,
            RiskDecisionStatus::RequiresOverride
        );
        assert!(outcome.dispatch.is_none());
        assert!(execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison")
            .is_empty());

        let journal_records = journal.list().expect("journal should list records");
        assert_eq!(journal_records.len(), 2);
        assert_eq!(journal_records[0].action, "decision");
        assert_eq!(journal_records[1].action, "hard_override_required");
        assert_eq!(
            journal_records[1].payload["reasons"][0],
            "broker-side stop-loss protection is unavailable"
        );
    }

    #[tokio::test]
    async fn active_override_is_journaled_and_allows_dispatch() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let mut strategy = sample_strategy();
        strategy.execution.broker_preferences.take_profit = BrokerPreference::BrokerRequired;

        let mut request = sample_runtime_execution_request(strategy);
        request.risk_state.broker_support.take_profit = false;
        request.risk_state.hard_override_active = true;
        request.execution.intent = ExecutionIntent::Enter {
            side: TradeSide::Buy,
            order_type: EntryOrderType::Market,
            quantity: 1,
            protective_brackets_expected: true,
            reason: "runtime entry".to_owned(),
        };

        let outcome =
            evaluate_risk_and_execute_tradovate(&request, &mut manager, &execution_api, &journal)
                .await
                .expect("active override should allow orchestration");

        assert_eq!(outcome.risk.decision.status, RiskDecisionStatus::Accepted);
        assert!(outcome.dispatch.is_some());
        assert_eq!(
            journal
                .list()
                .expect("journal should list records")
                .into_iter()
                .map(|record| record.action)
                .collect::<Vec<_>>(),
            vec!["decision".to_owned(), "hard_override_used".to_owned()]
        );
    }

    #[tokio::test]
    async fn manual_command_uses_unified_audited_path() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let mut request = sample_runtime_execution_request(sample_strategy());
        request.action_source = ActionSource::Dashboard;

        let outcome = RuntimeControlLoop::handle_command(
            RuntimeCommand::ManualIntent(request),
            &mut manager,
            &execution_api,
            &journal,
        )
        .await
        .expect("manual command should succeed");

        let RuntimeCommandOutcome::Execution(outcome) = outcome;
        assert!(outcome.dispatch.is_some());

        let records = journal.list().expect("journal should list records");
        assert_eq!(
            records
                .iter()
                .map(|record| record.action.as_str())
                .collect::<Vec<_>>(),
            vec!["intent_received", "decision", "dispatch_succeeded"]
        );
        assert_eq!(records[0].category, "manual");
        assert_eq!(records[0].source, ActionSource::Dashboard);
    }

    #[tokio::test]
    async fn strategy_command_normalizes_provenance_and_uses_same_path() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let mut request = sample_runtime_execution_request(sample_strategy());
        request.action_source = ActionSource::Dashboard;

        let outcome = RuntimeControlLoop::handle_command(
            RuntimeCommand::StrategyIntent(request),
            &mut manager,
            &execution_api,
            &journal,
        )
        .await
        .expect("strategy command should succeed");

        let RuntimeCommandOutcome::Execution(outcome) = outcome;
        assert!(outcome.dispatch.is_some());

        let records = journal.list().expect("journal should list records");
        assert_eq!(records[0].category, "strategy");
        assert_eq!(records[0].source, ActionSource::System);
        assert!(records
            .iter()
            .all(|record| record.source == ActionSource::System));
    }

    #[tokio::test]
    async fn manual_override_required_command_is_audited_without_dispatch() {
        let execution_api = FakeExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut manager = sample_session_manager().await;

        let mut strategy = sample_strategy();
        strategy.execution.broker_preferences.stop_loss = BrokerPreference::BrokerRequired;

        let mut request = sample_runtime_execution_request(strategy);
        request.action_source = ActionSource::Cli;
        request.risk_state.broker_support.stop_loss = false;

        let outcome = RuntimeControlLoop::handle_command(
            RuntimeCommand::ManualIntent(request),
            &mut manager,
            &execution_api,
            &journal,
        )
        .await
        .expect("override-required command should return a clean outcome");

        let RuntimeCommandOutcome::Execution(outcome) = outcome;
        assert!(outcome.dispatch.is_none());

        let records = journal.list().expect("journal should list records");
        assert_eq!(
            records
                .iter()
                .map(|record| record.action.as_str())
                .collect::<Vec<_>>(),
            vec![
                "intent_received",
                "decision",
                "hard_override_required",
                "dispatch_skipped",
            ]
        );
        assert_eq!(records[0].category, "manual");
        assert_eq!(records[0].source, ActionSource::Cli);
        assert_eq!(records[3].category, "execution");
    }
}
