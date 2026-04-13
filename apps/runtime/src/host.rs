use std::{
    collections::BTreeSet,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{watch, Mutex},
    time::{timeout, Duration},
};
use tracing::{error, info, warn};
use tv_bot_broker_tradovate::{
    TradovateCredentials, TradovateError, TradovateLiveClient, TradovateLiveClientConfig,
    TradovateRoutingPreferences, TradovateSessionConfig,
};
use tv_bot_config::AppConfig;
use tv_bot_control_api::{
    ControlApiCommand, ControlApiEventPublisher, HttpCommandHandler, HttpCommandRequest,
    HttpCommandResponse, HttpResponseBody, HttpStatusCode, LoadedStrategySummary, LocalControlApi,
    RuntimeCommandDispatcher, RuntimeHistorySnapshot, RuntimeJournalStatus,
    RuntimeKernelCommandDispatcher, RuntimeLifecycleCommand, RuntimeLifecycleRequest,
    RuntimeLifecycleResponse, RuntimeReadinessSnapshot, RuntimeReconnectDecision,
    RuntimeReconnectReviewStatus, RuntimeShutdownDecision, RuntimeShutdownReviewStatus,
    RuntimeStatusSnapshot, RuntimeStorageMode, RuntimeStorageStatus, RuntimeStrategyCatalogEntry,
    RuntimeStrategyIssue, RuntimeStrategyIssueSeverity, RuntimeStrategyLibraryResponse,
    RuntimeStrategyValidationRequest, RuntimeStrategyValidationResponse, WebSocketEventHub,
    WebSocketEventHubError, WebSocketEventStreamError,
};
use tv_bot_core_types::{
    ActionSource, BrokerStatusSnapshot, EventJournalRecord, EventSeverity, RuntimeMode,
    SystemHealthSnapshot, TradePathLatencyRecord, TradePathTimestamps,
};
use tv_bot_health::{
    RuntimeHealthError, RuntimeHealthInputs, RuntimeHealthSupervisor, RuntimeResourceSample,
    RuntimeResourceSampler, SysinfoRuntimeResourceSampler,
};
use tv_bot_journal::{EventJournal, JournalError, PersistentJournal, ProjectingJournal};
use tv_bot_market_data::{
    DatabentoLiveTransport, DatabentoLiveTransportConfig, DatabentoWarmupMode,
    MarketDataConnectionState, MarketDataService, MarketDataServiceSnapshot,
};
use tv_bot_metrics::{RuntimeLatencyCollector, RuntimeLatencyError};
use tv_bot_persistence::{
    PersistenceBackendKind, PersistenceRuntimeSelection, PersistenceStorageMode, RuntimePersistence,
};
use tv_bot_runtime_kernel::{
    RuntimeCommand, RuntimeCommandError, RuntimeCommandOutcome, RuntimeStateMachine,
};
use tv_bot_state_store::InMemoryStateStore;
use tv_bot_strategy_loader::{
    StrategyIssue, StrategyIssueSeverity, StrictStrategyCompiler,
};

use crate::history::{RuntimeBrokerSnapshot, RuntimeHistoryError, RuntimeHistoryRecorder};
use crate::operator::{
    LoadedStrategyMarketDataSeed, RuntimeOperatorError, RuntimeOperatorState, RuntimeStatusContext,
};

const EVENT_HUB_CAPACITY: usize = 256;
const MARKET_DATA_POLL_BUDGET: usize = 16;
const MARKET_DATA_POLL_TIMEOUT: Duration = Duration::from_millis(1);
const MARKET_DATA_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const HISTORY_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const HEALTH_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const SHUTDOWN_REVIEW_POLL_INTERVAL: Duration = Duration::from_millis(250);
type LiveRuntimeDispatcher = RuntimeKernelCommandDispatcher<
    TradovateLiveClient,
    TradovateLiveClient,
    TradovateLiveClient,
    tv_bot_broker_tradovate::SystemClock,
    TradovateLiveClient,
    ProjectingJournal<PersistentJournal, InMemoryStateStore>,
>;

#[derive(Clone, Debug)]
struct RuntimeMarketDataConfig {
    api_key: SecretString,
    gateway: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct RuntimeMarketDataView {
    snapshot: Option<MarketDataServiceSnapshot>,
    detail: Option<String>,
}

enum RuntimeMarketDataState {
    Unconfigured {
        detail: String,
    },
    PendingStrategy {
        detail: String,
    },
    StrategyBlocked {
        detail: String,
    },
    Active {
        service: MarketDataService<DatabentoLiveTransport>,
        last_snapshot: Option<MarketDataServiceSnapshot>,
    },
    #[cfg(test)]
    SnapshotOverride {
        snapshot: MarketDataServiceSnapshot,
        detail: Option<String>,
    },
}

struct RuntimeMarketDataManager {
    config: Option<RuntimeMarketDataConfig>,
    state: RuntimeMarketDataState,
}

#[derive(Clone, Debug, Default)]
struct ShutdownReviewState {
    pending_signal: bool,
    blocked: bool,
    awaiting_flatten: bool,
    decision: Option<RuntimeShutdownDecision>,
    reason: Option<String>,
    requested_at: Option<DateTime<Utc>>,
}

trait RuntimeDispatcherHandle: RuntimeCommandDispatcher {
    fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot;
    fn append_journal_record(&self, record: EventJournalRecord) -> Result<(), JournalError>;
    fn acknowledge_reconnect_review(
        &mut self,
        decision: RuntimeReconnectDecision,
    ) -> Result<(), String>;
}

struct BoxedDispatcher {
    inner: Box<dyn RuntimeDispatcherHandle + Send>,
    history: RuntimeHistoryRecorder,
    latency_collector: Arc<RuntimeLatencyCollector>,
    health_supervisor: Arc<RuntimeHealthSupervisor>,
    event_hub: WebSocketEventHub,
}

impl BoxedDispatcher {
    fn new(
        inner: Box<dyn RuntimeDispatcherHandle + Send>,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
        event_hub: WebSocketEventHub,
    ) -> Self {
        Self {
            inner,
            history,
            latency_collector,
            health_supervisor,
            event_hub,
        }
    }

    fn snapshot(&self) -> RuntimeBrokerSnapshot {
        self.inner.dispatch_snapshot()
    }

    fn append_journal_record(&self, record: EventJournalRecord) -> Result<(), JournalError> {
        self.inner.append_journal_record(record)
    }

    fn acknowledge_reconnect_review(
        &mut self,
        decision: RuntimeReconnectDecision,
    ) -> Result<(), String> {
        self.inner.acknowledge_reconnect_review(decision)
    }
}

#[async_trait]
impl RuntimeCommandDispatcher for BoxedDispatcher {
    async fn dispatch(
        &mut self,
        command: RuntimeCommand,
    ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
        let dispatch_started_at = Utc::now();
        let result = self.inner.dispatch(command.clone()).await;
        if let Ok(outcome) = &result {
            let snapshot = self.inner.dispatch_snapshot();
            let dispatch_finished_at = Utc::now();
            if let Some(record) = self.record_latency(
                &command,
                outcome,
                &snapshot,
                dispatch_started_at,
                dispatch_finished_at,
            ) {
                publish_trade_latency(&self.event_hub, &record);
            }

            let history_started_at = Instant::now();
            if let Err(error) =
                self.history
                    .record_execution_outcome(&command, outcome, &snapshot, Utc::now())
            {
                let _ = self.health_supervisor.note_error();
                warn!(?error, "failed to persist runtime execution history");
            } else {
                let _ = self
                    .health_supervisor
                    .record_db_write_latency(history_started_at.elapsed().as_millis() as u64);
            }
        } else {
            let _ = self.health_supervisor.note_error();
        }
        result
    }
}

impl BoxedDispatcher {
    fn record_latency(
        &self,
        command: &RuntimeCommand,
        outcome: &RuntimeCommandOutcome,
        snapshot: &RuntimeBrokerSnapshot,
        dispatch_started_at: chrono::DateTime<Utc>,
        dispatch_finished_at: chrono::DateTime<Utc>,
    ) -> Option<TradePathLatencyRecord> {
        let RuntimeCommandOutcome::Execution(outcome) = outcome;
        if outcome.dispatch.is_none() {
            return None;
        }

        let request = request_for_command(command);
        let latest_fill_at = snapshot.fills.iter().map(|fill| fill.occurred_at).max();
        let sync_update_at = snapshot
            .broker_status
            .as_ref()
            .and_then(|status| status.last_sync_at)
            .or_else(|| {
                snapshot
                    .account_snapshot
                    .as_ref()
                    .map(|account| account.captured_at)
            });

        let record = match self.latency_collector.record_trade_path(
            runtime_latency_action_id(request, dispatch_finished_at),
            Some(request.execution.strategy.metadata.strategy_id.clone()),
            TradePathTimestamps {
                market_event_at: None,
                signal_at: None,
                decision_at: Some(dispatch_started_at),
                order_sent_at: Some(dispatch_started_at),
                broker_ack_at: Some(dispatch_finished_at),
                fill_at: latest_fill_at.filter(|value| *value >= dispatch_started_at),
                sync_update_at: sync_update_at.filter(|value| *value >= dispatch_started_at),
            },
            dispatch_finished_at,
        ) {
            Ok(record) => record,
            Err(error) => {
                let _ = self.health_supervisor.note_error();
                warn!(?error, "failed to persist runtime latency metrics");
                return None;
            }
        };

        Some(record)
    }
}

impl RuntimeDispatcherHandle for LiveRuntimeDispatcher {
    fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot {
        let session = self.session().snapshot();

        RuntimeBrokerSnapshot {
            broker_status: Some(session.broker),
            last_reconnect_review_decision: session
                .last_review_decision
                .map(runtime_reconnect_decision_from_tradovate),
            account_snapshot: session.account_snapshot,
            open_positions: session.open_positions,
            working_orders: session.working_orders,
            fills: session.fills,
        }
    }

    fn append_journal_record(&self, record: EventJournalRecord) -> Result<(), JournalError> {
        self.journal().append(record)
    }

    fn acknowledge_reconnect_review(
        &mut self,
        decision: RuntimeReconnectDecision,
    ) -> Result<(), String> {
        self.session_mut()
            .acknowledge_reconnect_review(tradovate_reconnect_decision(decision));
        Ok(())
    }
}

impl RuntimeMarketDataManager {
    fn from_app_config(config: &AppConfig) -> Self {
        let Some(api_key) = config.market_data.api_key.clone() else {
            return Self {
                config: None,
                state: RuntimeMarketDataState::Unconfigured {
                    detail: "missing market-data configuration: market_data.api_key".to_owned(),
                },
            };
        };

        Self {
            config: Some(RuntimeMarketDataConfig {
                api_key,
                gateway: config.market_data.gateway.clone(),
            }),
            state: RuntimeMarketDataState::PendingStrategy {
                detail: "load a strategy to prepare the Databento market-data service".to_owned(),
            },
        }
    }

    fn configure_for_strategy(
        &mut self,
        seed: Option<LoadedStrategyMarketDataSeed>,
        now: chrono::DateTime<Utc>,
    ) {
        let Some(config) = self.config.clone() else {
            self.state = RuntimeMarketDataState::Unconfigured {
                detail: "missing market-data configuration: market_data.api_key".to_owned(),
            };
            return;
        };

        let Some(seed) = seed else {
            self.state = RuntimeMarketDataState::PendingStrategy {
                detail: "load a strategy to prepare the Databento market-data service".to_owned(),
            };
            return;
        };

        let Some(mapping) = seed.instrument_mapping else {
            self.state = RuntimeMarketDataState::StrategyBlocked {
                detail: seed.instrument_resolution_error.unwrap_or_else(|| {
                    "market-data service cannot start until strategy symbol resolution succeeds"
                        .to_owned()
                }),
            };
            return;
        };

        let mut transport_config = DatabentoLiveTransportConfig::new(config.api_key);
        if let Some(gateway) = config.gateway {
            transport_config = transport_config.with_gateway_address(gateway);
        }

        match MarketDataService::from_strategy(
            DatabentoLiveTransport::new(transport_config),
            &seed.strategy,
            &mapping,
            now,
        ) {
            Ok(service) => {
                let snapshot = service.snapshot(now);
                self.state = RuntimeMarketDataState::Active {
                    service,
                    last_snapshot: Some(snapshot),
                };
            }
            Err(error) => {
                self.state = RuntimeMarketDataState::StrategyBlocked {
                    detail: format!("failed to prepare market-data service: {error}"),
                };
            }
        }
    }

    async fn start_warmup(
        &mut self,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<MarketDataServiceSnapshot>, String> {
        match &mut self.state {
            RuntimeMarketDataState::Active {
                service,
                last_snapshot,
            } => service
                .start_warmup(DatabentoWarmupMode::LiveOnly, now)
                .await
                .map(|snapshot| {
                    *last_snapshot = Some(snapshot.clone());
                    Some(snapshot)
                })
                .map_err(|error| format!("market-data warmup start failed: {error}")),
            RuntimeMarketDataState::Unconfigured { detail }
            | RuntimeMarketDataState::PendingStrategy { detail }
            | RuntimeMarketDataState::StrategyBlocked { detail } => Err(detail.clone()),
            #[cfg(test)]
            RuntimeMarketDataState::SnapshotOverride { snapshot, .. } => {
                snapshot.warmup_requested = true;
                snapshot.warmup_mode = DatabentoWarmupMode::LiveOnly;
                snapshot.replay_caught_up = true;
                snapshot.session.market_data.warmup.started_at = Some(
                    snapshot
                        .session
                        .market_data
                        .warmup
                        .started_at
                        .unwrap_or(now),
                );
                snapshot.session.market_data.warmup.updated_at = now;
                snapshot.trade_ready = matches!(
                    snapshot.session.market_data.health,
                    tv_bot_market_data::MarketDataHealth::Healthy
                ) && matches!(
                    snapshot.session.market_data.warmup.status,
                    tv_bot_core_types::WarmupStatus::Ready
                );
                snapshot.updated_at = now;
                Ok(Some(snapshot.clone()))
            }
        }
    }

    async fn refresh(&mut self, now: chrono::DateTime<Utc>) -> RuntimeMarketDataView {
        match &mut self.state {
            RuntimeMarketDataState::Active {
                service,
                last_snapshot,
            } => {
                for _ in 0..MARKET_DATA_POLL_BUDGET {
                    match timeout(MARKET_DATA_POLL_TIMEOUT, service.poll_next_update()).await {
                        Ok(Ok(Some(_))) => continue,
                        Ok(Ok(None)) | Err(_) => break,
                        Ok(Err(error)) => {
                            service
                                .session_mut()
                                .coordinator_mut()
                                .set_connection_state(MarketDataConnectionState::Failed, now);
                            service.session_mut().coordinator_mut().mark_degraded(
                                format!("market-data service polling failed: {error}"),
                                now,
                            );
                            break;
                        }
                    }
                }

                let snapshot = service.snapshot(now);
                let detail = if matches!(
                    snapshot.session.market_data.connection_state,
                    MarketDataConnectionState::Failed
                ) {
                    Some(
                        snapshot
                            .session
                            .market_data
                            .feed_statuses
                            .iter()
                            .find(|status| {
                                matches!(
                                    status.state,
                                    tv_bot_market_data::FeedReadinessState::Degraded
                                )
                            })
                            .map(|status| status.detail.clone())
                            .unwrap_or_else(|| "market-data service reported a failure".to_owned()),
                    )
                } else {
                    None
                };
                *last_snapshot = Some(snapshot.clone());

                RuntimeMarketDataView {
                    snapshot: Some(snapshot),
                    detail,
                }
            }
            RuntimeMarketDataState::Unconfigured { detail }
            | RuntimeMarketDataState::PendingStrategy { detail }
            | RuntimeMarketDataState::StrategyBlocked { detail } => RuntimeMarketDataView {
                snapshot: None,
                detail: Some(detail.clone()),
            },
            #[cfg(test)]
            RuntimeMarketDataState::SnapshotOverride { snapshot, detail } => {
                RuntimeMarketDataView {
                    snapshot: Some(snapshot.clone()),
                    detail: detail.clone(),
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct RuntimeHostState {
    http_handler: Arc<Mutex<HttpCommandHandler<BoxedDispatcher, BestEffortEventPublisher>>>,
    history: RuntimeHistoryRecorder,
    latency_collector: Arc<RuntimeLatencyCollector>,
    health_supervisor: Arc<RuntimeHealthSupervisor>,
    resource_sampler: Arc<Mutex<Box<dyn RuntimeResourceSampler + Send>>>,
    market_data: Arc<Mutex<RuntimeMarketDataManager>>,
    event_hub: WebSocketEventHub,
    operator_state: Arc<Mutex<RuntimeOperatorState>>,
    http_bind: String,
    websocket_bind: String,
    command_dispatch_ready: bool,
    command_dispatch_detail: String,
    storage_status: RuntimeStorageStatus,
    journal_status: RuntimeJournalStatus,
    strategy_library_roots: Vec<PathBuf>,
    shutdown_signal: watch::Sender<bool>,
    shutdown_review: Arc<Mutex<ShutdownReviewState>>,
}

#[derive(Clone)]
struct BestEffortEventPublisher {
    hub: WebSocketEventHub,
}

impl ControlApiEventPublisher for BestEffortEventPublisher {
    fn publish(
        &self,
        event: tv_bot_control_api::ControlApiEvent,
    ) -> Result<(), WebSocketEventHubError> {
        match self.hub.publish(event) {
            Ok(()) | Err(WebSocketEventHubError::NoSubscribers) => Ok(()),
            Err(source) => Err(source),
        }
    }
}

struct UnavailableRuntimeCommandDispatcher {
    reason: String,
}

#[async_trait]
impl RuntimeCommandDispatcher for UnavailableRuntimeCommandDispatcher {
    async fn dispatch(
        &mut self,
        _command: RuntimeCommand,
    ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
        Err(RuntimeCommandError::Unavailable {
            reason: self.reason.clone(),
        })
    }
}

impl RuntimeDispatcherHandle for UnavailableRuntimeCommandDispatcher {
    fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot {
        RuntimeBrokerSnapshot::default()
    }

    fn append_journal_record(&self, _record: EventJournalRecord) -> Result<(), JournalError> {
        Ok(())
    }

    fn acknowledge_reconnect_review(
        &mut self,
        _decision: RuntimeReconnectDecision,
    ) -> Result<(), String> {
        Err(self.reason.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHostHealthResponse {
    pub status: String,
    pub system_health: Option<SystemHealthSnapshot>,
    pub latest_trade_latency: Option<TradePathLatencyRecord>,
}

#[derive(Debug, Error)]
pub enum RuntimeHostError {
    #[error("broker runtime setup failed: {source}")]
    BrokerSetup { source: TradovateError },
    #[error("journal runtime setup failed: {source}")]
    JournalSetup {
        #[source]
        source: JournalError,
    },
    #[error("history runtime setup failed: {source}")]
    HistorySetup {
        #[source]
        source: RuntimeHistoryError,
    },
    #[error("latency runtime setup failed: {source}")]
    LatencySetup {
        #[source]
        source: RuntimeLatencyError,
    },
    #[error("health runtime setup failed: {source}")]
    HealthSetup {
        #[source]
        source: RuntimeHealthError,
    },
    #[error("failed to create websocket event hub: {source}")]
    EventHub { source: WebSocketEventHubError },
    #[error("failed to bind {kind} listener on `{bind}`: {source}")]
    Bind {
        kind: &'static str,
        bind: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serve {kind} listener on `{bind}`: {source}")]
    Serve {
        kind: &'static str,
        bind: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to listen for shutdown signal: {source}")]
    ShutdownSignal {
        #[source]
        source: std::io::Error,
    },
    #[error("server task join failed: {0}")]
    Join(String),
}

pub async fn run_runtime_host(
    config: AppConfig,
    runtime: RuntimeStateMachine,
) -> Result<(), RuntimeHostError> {
    let state = build_runtime_host_state(&config, runtime)?;
    let mut shutdown_receiver = state.shutdown_signal.subscribe();
    let http_router = build_http_router(state.clone());
    let websocket_router = build_websocket_router(state.clone());

    let http_listener = bind_listener("http", &config.control_api.http_bind).await?;
    let ws_listener = bind_listener("websocket", &config.control_api.websocket_bind).await?;

    let http_local = http_listener
        .local_addr()
        .unwrap_or_else(|_| parse_fallback_addr(&config.control_api.http_bind));
    let ws_local = ws_listener
        .local_addr()
        .unwrap_or_else(|_| parse_fallback_addr(&config.control_api.websocket_bind));

    info!(
        http_bind = %http_local,
        websocket_bind = %ws_local,
        command_dispatch_ready = state.command_dispatch_ready,
        command_dispatch_detail = %state.command_dispatch_detail,
        "runtime host listening"
    );

    let http_bind = config.control_api.http_bind.clone();
    let ws_bind = config.control_api.websocket_bind.clone();
    let market_data_state = state.clone();
    let history_state = state.clone();
    let health_state = state.clone();

    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_router)
            .await
            .map_err(|source| RuntimeHostError::Serve {
                kind: "http",
                bind: http_bind,
                source,
            })
    });
    let ws_task = tokio::spawn(async move {
        axum::serve(ws_listener, websocket_router)
            .await
            .map_err(|source| RuntimeHostError::Serve {
                kind: "websocket",
                bind: ws_bind,
                source,
            })
    });
    let market_data_task = tokio::spawn(async move {
        market_data_refresh_loop(market_data_state).await;
        Ok::<(), RuntimeHostError>(())
    });
    let history_task = tokio::spawn(async move {
        history_refresh_loop(history_state).await;
        Ok::<(), RuntimeHostError>(())
    });
    let health_task = tokio::spawn(async move {
        health_refresh_loop(health_state).await;
        Ok::<(), RuntimeHostError>(())
    });

    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|source| RuntimeHostError::ShutdownSignal { source })?;
                info!("shutdown signal received");

                if handle_runtime_shutdown_signal(&state).await {
                    break;
                }
            }
            changed = shutdown_receiver.changed() => {
                if changed.is_ok() && *shutdown_receiver.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(SHUTDOWN_REVIEW_POLL_INTERVAL) => {
                if finalize_pending_flatten_shutdown(&state).await {
                    break;
                }
            }
        }
    }

    journal_host_event(
        &state,
        "runtime",
        "shutdown_completed",
        ActionSource::System,
        EventSeverity::Info,
        json!({
            "reason": "runtime host shutdown approved",
        }),
    )
    .await;

    if let Err(error) = state
        .history
        .record_run_status(tv_bot_core_types::StrategyRunStatus::Cancelled, Utc::now())
    {
        let _ = state.health_supervisor.note_error();
        warn!(?error, "failed to persist shutdown run status");
    }

    http_task.abort();
    ws_task.abort();
    market_data_task.abort();
    history_task.abort();
    health_task.abort();

    match http_task.await {
        Ok(Err(source)) => return Err(source),
        Err(error) if !error.is_cancelled() => {
            return Err(RuntimeHostError::Join(error.to_string()))
        }
        _ => {}
    }
    match ws_task.await {
        Ok(Err(source)) => return Err(source),
        Err(error) if !error.is_cancelled() => {
            return Err(RuntimeHostError::Join(error.to_string()))
        }
        _ => {}
    }
    match market_data_task.await {
        Ok(Err(source)) => return Err(source),
        Err(error) if !error.is_cancelled() => {
            return Err(RuntimeHostError::Join(error.to_string()))
        }
        _ => {}
    }
    match history_task.await {
        Ok(Err(source)) => return Err(source),
        Err(error) if !error.is_cancelled() => {
            return Err(RuntimeHostError::Join(error.to_string()))
        }
        _ => {}
    }
    match health_task.await {
        Ok(Err(source)) => return Err(source),
        Err(error) if !error.is_cancelled() => {
            return Err(RuntimeHostError::Join(error.to_string()))
        }
        _ => {}
    }

    Ok(())
}

pub fn build_http_router(state: RuntimeHostState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/readiness", get(readiness_handler))
        .route("/history", get(history_handler))
        .route("/strategies", get(strategy_library_handler))
        .route("/strategies/validate", post(strategy_validation_handler))
        .route("/runtime/commands", post(runtime_command_handler))
        .route("/commands", post(command_handler))
        .with_state(state)
}

pub fn build_websocket_router(state: RuntimeHostState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/events", get(websocket_handler))
        .with_state(state)
}

pub fn build_runtime_host_state(
    config: &AppConfig,
    runtime: RuntimeStateMachine,
) -> Result<RuntimeHostState, RuntimeHostError> {
    let persistence = RuntimePersistence::open(config);
    let persistence_selection = persistence.selection().clone();
    let history = RuntimeHistoryRecorder::from_persistence(&persistence)
        .map_err(|source| RuntimeHostError::HistorySetup { source })?;
    let latency_collector = Arc::new(
        RuntimeLatencyCollector::from_persistence(&persistence)
            .map_err(|source| RuntimeHostError::LatencySetup { source })?,
    );
    let health_supervisor = Arc::new(
        RuntimeHealthSupervisor::from_persistence(&persistence)
            .map_err(|source| RuntimeHostError::HealthSetup { source })?,
    );
    let journal = ProjectingJournal::with_hydrated_projection(
        PersistentJournal::new(persistence.event_store()),
        InMemoryStateStore::new(),
    )
    .map_err(|source| RuntimeHostError::JournalSetup { source })?;
    let journal_status = build_journal_status(&persistence_selection);
    let storage_status = build_storage_status(&persistence_selection);
    let strategy_library_roots = discover_strategy_library_roots(config);
    let event_hub = WebSocketEventHub::new(EVENT_HUB_CAPACITY)
        .map_err(|source| RuntimeHostError::EventHub { source })?;
    let (shutdown_signal, _) = watch::channel(false);
    let (dispatcher, command_dispatch_ready, command_dispatch_detail) = build_dispatcher(
        config,
        journal,
        history.clone(),
        latency_collector.clone(),
        health_supervisor.clone(),
        event_hub.clone(),
    )?;

    if persistence_selection.durable {
        info!(
            backend = %persistence_selection.active_backend.as_str(),
            fallback_activated = persistence_selection.fallback_activated,
            detail = %persistence_selection.detail,
            "runtime persistence backend selected"
        );
    } else {
        warn!(
            backend = %persistence_selection.active_backend.as_str(),
            detail = %persistence_selection.detail,
            "runtime persistence is not durable"
        );
    }

    if !command_dispatch_ready {
        warn!(
            detail = %command_dispatch_detail,
            "runtime host started with command dispatch unavailable"
        );
    }

    let handler = HttpCommandHandler::with_publisher(
        LocalControlApi::new(dispatcher),
        BestEffortEventPublisher {
            hub: event_hub.clone(),
        },
    );
    let operator_state = RuntimeOperatorState::new(runtime);

    Ok(RuntimeHostState {
        http_handler: Arc::new(Mutex::new(handler)),
        history,
        latency_collector,
        health_supervisor,
        resource_sampler: Arc::new(Mutex::new(Box::new(
            SysinfoRuntimeResourceSampler::new_current_process(),
        ))),
        market_data: Arc::new(Mutex::new(RuntimeMarketDataManager::from_app_config(
            config,
        ))),
        event_hub,
        operator_state: Arc::new(Mutex::new(operator_state)),
        http_bind: config.control_api.http_bind.clone(),
        websocket_bind: config.control_api.websocket_bind.clone(),
        command_dispatch_ready,
        command_dispatch_detail,
        storage_status,
        journal_status,
        strategy_library_roots,
        shutdown_signal,
        shutdown_review: Arc::new(Mutex::new(ShutdownReviewState::default())),
    })
}

fn discover_strategy_library_roots(config: &AppConfig) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(path) = config.runtime.default_strategy_path.as_ref() {
        if let Some(parent) = path.parent() {
            push_strategy_library_root(parent, &mut roots, &mut seen);
        }
    }

    if let Some(examples_root) = discover_examples_root() {
        push_strategy_library_root(&examples_root, &mut roots, &mut seen);
    }

    roots
}

fn discover_examples_root() -> Option<PathBuf> {
    let mut current = std::env::current_dir().ok()?;

    loop {
        let candidate = current.join("strategies").join("examples");
        if candidate.is_dir() {
            return Some(candidate);
        }

        if !current.pop() {
            break;
        }
    }

    None
}

fn push_strategy_library_root(
    candidate: &Path,
    roots: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) {
    if !candidate.is_dir() {
        return;
    }

    let normalized = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    if seen.insert(normalized.clone()) {
        roots.push(normalized);
    }
}

fn display_strategy_path(path: &Path) -> String {
    if let Ok(current_dir) = std::env::current_dir() {
        if let Ok(relative) = path.strip_prefix(&current_dir) {
            return relative.display().to_string();
        }
    }

    path.display().to_string()
}

async fn bind_listener(kind: &'static str, bind: &str) -> Result<TcpListener, RuntimeHostError> {
    TcpListener::bind(bind)
        .await
        .map_err(|source| RuntimeHostError::Bind {
            kind,
            bind: bind.to_owned(),
            source,
        })
}

async fn health_handler(State(state): State<RuntimeHostState>) -> Json<RuntimeHostHealthResponse> {
    let system_health = state.health_supervisor.snapshot().unwrap_or(None);
    let latest_trade_latency = state
        .latency_collector
        .snapshot()
        .ok()
        .and_then(|snapshot| snapshot.latest_record);

    Json(RuntimeHostHealthResponse {
        status: host_health_status(&state, system_health.as_ref()),
        system_health,
        latest_trade_latency,
    })
}

async fn status_handler(State(state): State<RuntimeHostState>) -> Json<RuntimeStatusSnapshot> {
    sync_history_state(&state).await;
    let context = status_context(&state, true).await;
    let operator = state.operator_state.lock().await;
    Json(operator.status_snapshot(&context))
}

async fn readiness_handler(
    State(state): State<RuntimeHostState>,
) -> Json<RuntimeReadinessSnapshot> {
    sync_history_state(&state).await;
    let context = status_context(&state, true).await;
    let operator = state.operator_state.lock().await;
    Json(operator.readiness_snapshot(&context))
}

async fn history_handler(State(state): State<RuntimeHostState>) -> Response {
    sync_history_state(&state).await;

    match state.history.snapshot() {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            error!(?error, "runtime host history handler failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(HttpCommandResponse {
                    status_code: HttpStatusCode::InternalServerError,
                    body: tv_bot_control_api::HttpResponseBody::Error {
                        message: error.to_string(),
                    },
                }),
            )
                .into_response()
        }
    }
}

async fn strategy_library_handler(State(state): State<RuntimeHostState>) -> Response {
    let roots = state.strategy_library_roots.clone();

    match tokio::task::spawn_blocking(move || scan_strategy_library(roots)).await {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(error)) => runtime_host_error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
        Err(error) => runtime_host_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("strategy library scan task failed: {error}"),
        ),
    }
}

async fn strategy_validation_handler(
    State(state): State<RuntimeHostState>,
    Json(request): Json<RuntimeStrategyValidationRequest>,
) -> Response {
    let path = request.path.clone();

    match tokio::task::spawn_blocking(move || validate_strategy_path(path)).await {
        Ok(Ok(response)) => {
            let source = request.source.into();
            let severity = if response.valid {
                EventSeverity::Info
            } else {
                EventSeverity::Warning
            };
            let action = if response.valid {
                "validation_succeeded"
            } else {
                "validation_failed"
            };

            journal_host_event(
                &state,
                "strategy",
                action,
                source,
                severity,
                json!({
                    "path": response.display_path,
                    "valid": response.valid,
                    "warning_count": response.warnings.len(),
                    "error_count": response.errors.len(),
                }),
            )
            .await;

            Json(response).into_response()
        }
        Ok(Err(error)) => runtime_host_error_response(StatusCode::INTERNAL_SERVER_ERROR, error),
        Err(error) => runtime_host_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("strategy validation task failed: {error}"),
        ),
    }
}

async fn runtime_command_handler(
    State(state): State<RuntimeHostState>,
    Json(request): Json<RuntimeLifecycleRequest>,
) -> Response {
    match request.command {
        RuntimeLifecycleCommand::LoadStrategy { path } => {
            load_strategy_runtime_command_handler(state, path, request.source).await
        }
        RuntimeLifecycleCommand::StartWarmup => {
            start_warmup_runtime_command_handler(state, request.source).await
        }
        RuntimeLifecycleCommand::ResolveReconnectReview {
            decision,
            contract_id,
            reason,
        } => {
            reconnect_review_runtime_command_handler(
                state,
                request.source,
                decision,
                contract_id,
                reason,
            )
            .await
        }
        RuntimeLifecycleCommand::Shutdown {
            decision,
            contract_id,
            reason,
        } => {
            shutdown_runtime_command_handler(state, request.source, decision, contract_id, reason)
                .await
        }
        RuntimeLifecycleCommand::Flatten {
            contract_id,
            reason,
        } => flatten_runtime_command_handler(state, request.source, contract_id, reason).await,
        command => lifecycle_state_command_handler(state, command, request.source).await,
    }
}

async fn lifecycle_state_command_handler(
    state: RuntimeHostState,
    command: RuntimeLifecycleCommand,
    source: tv_bot_control_api::ManualCommandSource,
) -> Response {
    let context = status_context(&state, true).await;
    let history_command = command.clone();
    let command_result = {
        let mut operator = state.operator_state.lock().await;
        operator.apply_lifecycle_command(command, &context)
    };
    let message = match command_result {
        Ok(message) => message,
        Err(error) => {
            return runtime_lifecycle_error_response(&state, error).await;
        }
    };

    if let Err(error) = sync_history_for_lifecycle_command(&state, &history_command, source).await {
        let _ = state.health_supervisor.note_error();
        warn!(?error, "failed to persist lifecycle history");
    }

    runtime_lifecycle_success_response(&state, HttpStatusCode::Ok, message, None).await
}

async fn load_strategy_runtime_command_handler(
    state: RuntimeHostState,
    path: std::path::PathBuf,
    source: tv_bot_control_api::ManualCommandSource,
) -> Response {
    let context = status_context(&state, true).await;
    let command_result = {
        let mut operator = state.operator_state.lock().await;
        let message = operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::LoadStrategy { path }, &context);
        let market_data_seed = operator.market_data_seed().ok();
        let current_mode = operator.status_snapshot(&context).mode;
        (message, market_data_seed, current_mode)
    };
    let (message, market_data_seed, current_mode) = match command_result {
        (Ok(message), market_data_seed, current_mode) => (message, market_data_seed, current_mode),
        (Err(error), _, _) => return runtime_lifecycle_error_response(&state, error).await,
    };

    {
        let mut market_data = state.market_data.lock().await;
        market_data.configure_for_strategy(market_data_seed.clone(), Utc::now());
    }

    if let Some(seed) = market_data_seed {
        match state.history.record_strategy_loaded(
            seed.strategy.metadata.strategy_id.clone(),
            current_mode,
            source.into(),
            Utc::now(),
        ) {
            Ok(Some(snapshot)) => publish_history_snapshot(&state, &snapshot).await,
            Ok(None) => {}
            Err(error) => {
                let _ = state.health_supervisor.note_error();
                warn!(?error, "failed to persist strategy load history");
            }
        }
    }

    runtime_lifecycle_success_response(&state, HttpStatusCode::Ok, message, None).await
}

async fn start_warmup_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
) -> Response {
    let market_data_result = {
        let mut market_data = state.market_data.lock().await;
        market_data.start_warmup(Utc::now()).await
    };

    if let Err(message) = market_data_result {
        return runtime_lifecycle_success_response(&state, HttpStatusCode::Conflict, message, None)
            .await;
    }

    let context = status_context(&state, true).await;
    let command_result = {
        let mut operator = state.operator_state.lock().await;
        operator.apply_lifecycle_command(RuntimeLifecycleCommand::StartWarmup, &context)
    };
    let message = match command_result {
        Ok(message) => message,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    if let Err(error) =
        sync_history_for_lifecycle_command(&state, &RuntimeLifecycleCommand::StartWarmup, source)
            .await
    {
        let _ = state.health_supervisor.note_error();
        warn!(?error, "failed to persist warmup history");
    }

    runtime_lifecycle_success_response(&state, HttpStatusCode::Ok, message, None).await
}

async fn flatten_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
    contract_id: i64,
    reason: String,
) -> Response {
    let context = status_context(&state, true).await;
    let request_result = {
        let operator = state.operator_state.lock().await;
        operator.build_flatten_request(&context, source, contract_id, reason)
    };
    let request = match request_result {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    dispatch_lifecycle_execution_request(&state, request, "flatten command dispatched".to_owned())
        .await
}

async fn reconnect_review_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
    decision: RuntimeReconnectDecision,
    contract_id: Option<i64>,
    reason: Option<String>,
) -> Response {
    let context = status_context(&state, true).await;
    if !context.reconnect_review.required {
        return runtime_lifecycle_success_response(
            &state,
            HttpStatusCode::Conflict,
            "broker reconnect review is not currently required".to_owned(),
            None,
        )
        .await;
    }

    match decision {
        RuntimeReconnectDecision::ClosePosition => {
            let contract_id_result = {
                let operator = state.operator_state.lock().await;
                operator.resolve_active_contract_id(&context, contract_id)
            };
            let contract_id = match contract_id_result {
                Ok(contract_id) => contract_id,
                Err(error) => return runtime_lifecycle_error_response(&state, error).await,
            };
            let reason = reason.unwrap_or_else(|| {
                "reconnect review requested closing the broker position".to_owned()
            });

            journal_host_event(
                &state,
                "broker",
                "reconnect_review_close_requested",
                source.into(),
                EventSeverity::Warning,
                json!({
                    "decision": decision,
                    "contract_id": contract_id,
                    "reason": reason,
                }),
            )
            .await;

            let request_result = {
                let operator = state.operator_state.lock().await;
                operator.build_flatten_request(&context, source, contract_id, reason)
            };
            let request = match request_result {
                Ok(request) => request,
                Err(error) => return runtime_lifecycle_error_response(&state, error).await,
            };

            dispatch_lifecycle_execution_request(
                &state,
                request,
                "reconnect close command dispatched".to_owned(),
            )
            .await
        }
        RuntimeReconnectDecision::LeaveBrokerProtected
        | RuntimeReconnectDecision::ReattachBotManagement => {
            let acknowledgement = {
                let mut handler = state.http_handler.lock().await;
                handler
                    .dispatcher_mut()
                    .acknowledge_reconnect_review(decision)
            };
            if let Err(message) = acknowledgement {
                return runtime_lifecycle_success_response(
                    &state,
                    HttpStatusCode::Conflict,
                    message,
                    None,
                )
                .await;
            }

            journal_host_event(
                &state,
                "broker",
                "reconnect_review_resolved",
                source.into(),
                EventSeverity::Warning,
                json!({
                    "decision": decision,
                    "reason": reason,
                    "open_position_count": context.reconnect_review.open_position_count,
                    "working_order_count": context.reconnect_review.working_order_count,
                }),
            )
            .await;

            sync_history_state(&state).await;

            runtime_lifecycle_success_response(
                &state,
                HttpStatusCode::Ok,
                format!(
                    "reconnect review resolved with {}",
                    reconnect_decision_label(decision)
                ),
                None,
            )
            .await
        }
    }
}

async fn shutdown_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
    decision: RuntimeShutdownDecision,
    contract_id: Option<i64>,
    reason: Option<String>,
) -> Response {
    let context = status_context(&state, true).await;
    let open_position_count = active_open_position_count(&context.open_positions);

    if open_position_count == 0 {
        let message = "shutdown approved; no open broker position is active".to_owned();
        approve_runtime_shutdown(
            &state,
            decision,
            message.clone(),
            source.into(),
            json!({
                "decision": decision,
                "open_position_count": 0,
            }),
        )
        .await;

        return runtime_lifecycle_success_response(&state, HttpStatusCode::Ok, message, None).await;
    }

    match decision {
        RuntimeShutdownDecision::LeaveBrokerProtected => {
            let all_broker_protected = {
                let operator = state.operator_state.lock().await;
                operator.all_open_positions_broker_protected(&context)
            };

            if !all_broker_protected {
                let message = "shutdown cannot leave positions in place because not all open positions report broker-side protection".to_owned();
                block_runtime_shutdown(&state, message.clone(), true).await;
                journal_host_event(
                    &state,
                    "runtime",
                    "shutdown_blocked",
                    source.into(),
                    EventSeverity::Warning,
                    json!({
                        "decision": decision,
                        "reason": message,
                        "open_position_count": open_position_count,
                        "all_positions_broker_protected": false,
                    }),
                )
                .await;

                return runtime_lifecycle_success_response(
                    &state,
                    HttpStatusCode::Conflict,
                    message,
                    None,
                )
                .await;
            }

            let message = format!(
                "shutdown approved; leaving {open_position_count} broker-protected open position(s) in place"
            );
            approve_runtime_shutdown(
                &state,
                decision,
                message.clone(),
                source.into(),
                json!({
                    "decision": decision,
                    "open_position_count": open_position_count,
                    "all_positions_broker_protected": true,
                }),
            )
            .await;

            runtime_lifecycle_success_response(&state, HttpStatusCode::Ok, message, None).await
        }
        RuntimeShutdownDecision::FlattenFirst => {
            let contract_id_result = {
                let operator = state.operator_state.lock().await;
                operator.resolve_active_contract_id(&context, contract_id)
            };
            let contract_id = match contract_id_result {
                Ok(contract_id) => contract_id,
                Err(error) => return runtime_lifecycle_error_response(&state, error).await,
            };
            let reason =
                reason.unwrap_or_else(|| "shutdown requested flatten before exit".to_owned());

            journal_host_event(
                &state,
                "runtime",
                "shutdown_flatten_requested",
                source.into(),
                EventSeverity::Warning,
                json!({
                    "decision": decision,
                    "contract_id": contract_id,
                    "reason": reason,
                    "open_position_count": open_position_count,
                }),
            )
            .await;

            let request_result = {
                let operator = state.operator_state.lock().await;
                operator.build_flatten_request(&context, source, contract_id, reason)
            };
            let request = match request_result {
                Ok(request) => request,
                Err(error) => return runtime_lifecycle_error_response(&state, error).await,
            };

            let (status_code, command_result, error_message) =
                match execute_lifecycle_execution_request(&state, request).await {
                    Ok(result) => result,
                    Err(message) => {
                        return runtime_lifecycle_success_response(
                            &state,
                            HttpStatusCode::InternalServerError,
                            message,
                            None,
                        )
                        .await;
                    }
                };

            if let Some(command_result) = command_result {
                mark_shutdown_waiting_for_flatten(
                    &state,
                    format!(
                        "shutdown is waiting for flatten confirmation on {open_position_count} open position(s)"
                    ),
                )
                .await;

                return runtime_lifecycle_success_response(
                    &state,
                    status_code,
                    "shutdown will continue after the broker position is flat".to_owned(),
                    Some(command_result),
                )
                .await;
            }

            runtime_lifecycle_success_response(&state, status_code, error_message, None).await
        }
    }
}

async fn dispatch_lifecycle_execution_request(
    state: &RuntimeHostState,
    request: HttpCommandRequest,
    success_message: String,
) -> Response {
    let (status_code, command_result, error_message) =
        match execute_lifecycle_execution_request(state, request).await {
            Ok(result) => result,
            Err(message) => {
                return runtime_lifecycle_success_response(
                    state,
                    HttpStatusCode::InternalServerError,
                    message,
                    None,
                )
                .await;
            }
        };

    if let Some(command_result) = command_result {
        return runtime_lifecycle_success_response(
            state,
            status_code,
            success_message,
            Some(command_result),
        )
        .await;
    }

    runtime_lifecycle_success_response(state, status_code, error_message, None).await
}

async fn execute_lifecycle_execution_request(
    state: &RuntimeHostState,
    request: HttpCommandRequest,
) -> Result<
    (
        HttpStatusCode,
        Option<tv_bot_control_api::ControlApiCommandResult>,
        String,
    ),
    String,
> {
    let mut handler = state.http_handler.lock().await;
    match handler.handle_command(request).await {
        Ok(response) => {
            drop(handler);
            sync_history_state(state).await;
            let result = match response.body {
                HttpResponseBody::CommandResult(result) => {
                    (response.status_code, Some(result), String::new())
                }
                HttpResponseBody::Error { message } => (response.status_code, None, message),
            };
            Ok(result)
        }
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            error!(?error, "runtime host lifecycle execution command failed");
            Err(error.to_string())
        }
    }
}

async fn command_handler(
    State(state): State<RuntimeHostState>,
    Json(request): Json<HttpCommandRequest>,
) -> Response {
    let context = status_context(&state, true).await;
    let request_result = {
        let operator = state.operator_state.lock().await;
        operator.sanitize_command_request(&context, request)
    };
    let request = match request_result {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    let mut handler = state.http_handler.lock().await;
    match handler.handle_command(request).await {
        Ok(response) => {
            drop(handler);
            sync_history_state(&state).await;
            (status_code(response.status_code), Json(response)).into_response()
        }
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            error!(?error, "runtime host command handler failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(HttpCommandResponse {
                    status_code: HttpStatusCode::InternalServerError,
                    body: tv_bot_control_api::HttpResponseBody::Error {
                        message: error.to_string(),
                    },
                }),
            )
                .into_response()
        }
    }
}

async fn websocket_handler(
    State(state): State<RuntimeHostState>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let stream = state.event_hub.subscribe();
    upgrade.on_upgrade(move |socket| websocket_event_loop(socket, stream))
}

async fn websocket_event_loop(
    mut socket: WebSocket,
    mut stream: tv_bot_control_api::WebSocketEventStream,
) {
    loop {
        match stream.recv().await {
            Ok(event) => {
                let payload = match serde_json::to_string(&event) {
                    Ok(payload) => payload,
                    Err(error) => {
                        error!(?error, "failed to serialize websocket control event");
                        continue;
                    }
                };

                if socket.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            Err(WebSocketEventStreamError::Lagged { skipped }) => {
                warn!(skipped, "websocket event stream lagged");
            }
            Err(WebSocketEventStreamError::Closed) => break,
        }
    }
}

fn build_dispatcher(
    config: &AppConfig,
    journal: ProjectingJournal<PersistentJournal, InMemoryStateStore>,
    history: RuntimeHistoryRecorder,
    latency_collector: Arc<RuntimeLatencyCollector>,
    health_supervisor: Arc<RuntimeHealthSupervisor>,
    event_hub: WebSocketEventHub,
) -> Result<(BoxedDispatcher, bool, String), RuntimeHostError> {
    let missing_fields = missing_broker_fields(config);
    if !missing_fields.is_empty() {
        let reason = format!(
            "missing broker configuration: {}",
            missing_fields.join(", ")
        );
        return Ok((
            BoxedDispatcher::new(
                Box::new(UnavailableRuntimeCommandDispatcher {
                    reason: reason.clone(),
                }),
                history,
                latency_collector,
                health_supervisor,
                event_hub,
            ),
            false,
            reason,
        ));
    }

    let session_config = TradovateSessionConfig::new(
        config
            .broker
            .environment
            .expect("missing fields checked above"),
        config
            .broker
            .http_base_url
            .clone()
            .expect("missing fields checked above"),
        config
            .broker
            .websocket_url
            .clone()
            .expect("missing fields checked above"),
    )
    .map_err(|source| RuntimeHostError::BrokerSetup { source })?;

    let credentials = TradovateCredentials {
        username: config
            .broker
            .username
            .clone()
            .expect("missing fields checked above"),
        password: config
            .broker
            .password
            .clone()
            .expect("missing fields checked above"),
        cid: config
            .broker
            .cid
            .clone()
            .expect("missing fields checked above"),
        sec: config
            .broker
            .sec
            .clone()
            .expect("missing fields checked above"),
        app_id: config
            .broker
            .app_id
            .clone()
            .expect("missing fields checked above"),
        app_version: config
            .broker
            .app_version
            .clone()
            .expect("missing fields checked above"),
        device_id: config.broker.device_id.clone(),
    };
    let routing_preferences = TradovateRoutingPreferences {
        paper_account_name: config.broker.paper_account_name.clone(),
        live_account_name: config.broker.live_account_name.clone(),
    };
    let live_client_config = TradovateLiveClientConfig::default();

    let session = tv_bot_broker_tradovate::TradovateSessionManager::with_system_clock(
        session_config,
        credentials,
        routing_preferences,
        TradovateLiveClient::new(live_client_config.clone()),
        TradovateLiveClient::new(live_client_config.clone()),
        TradovateLiveClient::new(live_client_config),
    )
    .map_err(|source| RuntimeHostError::BrokerSetup { source })?;
    let execution_api = TradovateLiveClient::new(TradovateLiveClientConfig::default());
    let dispatcher = RuntimeKernelCommandDispatcher::new(session, execution_api, journal);

    Ok((
        BoxedDispatcher::new(
            Box::new(dispatcher),
            history,
            latency_collector,
            health_supervisor,
            event_hub,
        ),
        true,
        "tradovate runtime command dispatch configured".to_owned(),
    ))
}

fn missing_broker_fields(config: &AppConfig) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if config.broker.environment.is_none() {
        missing.push("broker.environment");
    }
    if option_str_is_missing(config.broker.http_base_url.as_deref()) {
        missing.push("broker.http_base_url");
    }
    if option_str_is_missing(config.broker.websocket_url.as_deref()) {
        missing.push("broker.websocket_url");
    }
    if option_str_is_missing(config.broker.username.as_deref()) {
        missing.push("broker.username");
    }
    if config.broker.password.is_none() {
        missing.push("broker.password");
    }
    if option_str_is_missing(config.broker.cid.as_deref()) {
        missing.push("broker.cid");
    }
    if config.broker.sec.is_none() {
        missing.push("broker.sec");
    }
    if option_str_is_missing(config.broker.app_id.as_deref()) {
        missing.push("broker.app_id");
    }
    if option_str_is_missing(config.broker.app_version.as_deref()) {
        missing.push("broker.app_version");
    }
    missing
}

fn option_str_is_missing(value: Option<&str>) -> bool {
    match value {
        Some(text) => text.is_empty(),
        None => true,
    }
}

fn build_storage_status(selection: &PersistenceRuntimeSelection) -> RuntimeStorageStatus {
    RuntimeStorageStatus {
        mode: match selection.plan.mode {
            PersistenceStorageMode::Unconfigured => RuntimeStorageMode::Unconfigured,
            PersistenceStorageMode::PrimaryConfigured => RuntimeStorageMode::PrimaryConfigured,
            PersistenceStorageMode::SqliteFallbackOnly => RuntimeStorageMode::SqliteFallbackOnly,
        },
        primary_configured: selection.plan.primary_configured,
        sqlite_fallback_enabled: selection.plan.sqlite_fallback_enabled,
        sqlite_path: selection.plan.sqlite_path.clone(),
        allow_runtime_fallback: selection.plan.allow_runtime_fallback,
        active_backend: selection.active_backend.as_str().to_owned(),
        durable: selection.durable,
        fallback_activated: selection.fallback_activated,
        detail: selection.detail.clone(),
    }
}

fn build_journal_status(selection: &PersistenceRuntimeSelection) -> RuntimeJournalStatus {
    RuntimeJournalStatus {
        backend: selection.active_backend.as_str().to_owned(),
        durable: selection.durable,
        detail: match selection.active_backend {
            PersistenceBackendKind::Postgres => {
                "event journal records are durably persisted to Postgres".to_owned()
            }
            PersistenceBackendKind::Sqlite if selection.fallback_activated => {
                "event journal records are durably persisted to SQLite fallback because primary Postgres is unavailable".to_owned()
            }
            PersistenceBackendKind::Sqlite => {
                "event journal records are durably persisted to SQLite".to_owned()
            }
            PersistenceBackendKind::InMemory => {
                "event journal records are retained in memory only".to_owned()
            }
        },
    }
}

fn status_code(code: HttpStatusCode) -> StatusCode {
    match code {
        HttpStatusCode::Ok => StatusCode::OK,
        HttpStatusCode::Conflict => StatusCode::CONFLICT,
        HttpStatusCode::PreconditionRequired => StatusCode::PRECONDITION_REQUIRED,
        HttpStatusCode::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn status_context(
    state: &RuntimeHostState,
    sync_market_data_warmup: bool,
) -> RuntimeStatusContext {
    let dispatch_snapshot = current_dispatch_snapshot(state).await;
    let market_data_view = refresh_market_data_view(state, sync_market_data_warmup).await;
    sync_system_health(state, &dispatch_snapshot, &market_data_view).await;
    let latency_snapshot = state.latency_collector.snapshot().unwrap_or_default();
    let reconnect_review = reconnect_review_status(&dispatch_snapshot);
    let shutdown_review = shutdown_review_status(state, &dispatch_snapshot).await;

    RuntimeStatusContext {
        http_bind: state.http_bind.clone(),
        websocket_bind: state.websocket_bind.clone(),
        command_dispatch_ready: state.command_dispatch_ready,
        command_dispatch_detail: state.command_dispatch_detail.clone(),
        broker_status: dispatch_snapshot.broker_status,
        market_data_status: market_data_view.snapshot,
        market_data_detail: market_data_view.detail,
        storage_status: state.storage_status.clone(),
        journal_status: state.journal_status.clone(),
        system_health: state.health_supervisor.snapshot().unwrap_or(None),
        latest_trade_latency: latency_snapshot.latest_record,
        recorded_trade_latency_count: latency_snapshot.total_records,
        open_positions: dispatch_snapshot.open_positions,
        working_orders: dispatch_snapshot.working_orders,
        reconnect_review,
        shutdown_review,
    }
}

fn reconnect_review_status(
    dispatch_snapshot: &RuntimeBrokerSnapshot,
) -> RuntimeReconnectReviewStatus {
    let required = dispatch_snapshot
        .broker_status
        .as_ref()
        .map(|snapshot| {
            matches!(
                snapshot.sync_state,
                tv_bot_core_types::BrokerSyncState::ReviewRequired
            ) || snapshot.review_required_reason.is_some()
        })
        .unwrap_or(false);

    RuntimeReconnectReviewStatus {
        required,
        reason: dispatch_snapshot
            .broker_status
            .as_ref()
            .and_then(|snapshot| snapshot.review_required_reason.clone()),
        last_decision: dispatch_snapshot.last_reconnect_review_decision,
        open_position_count: active_open_position_count(&dispatch_snapshot.open_positions),
        working_order_count: dispatch_snapshot.working_orders.len(),
    }
}

async fn shutdown_review_status(
    state: &RuntimeHostState,
    dispatch_snapshot: &RuntimeBrokerSnapshot,
) -> RuntimeShutdownReviewStatus {
    let review = state.shutdown_review.lock().await.clone();

    RuntimeShutdownReviewStatus {
        pending_signal: review.pending_signal,
        blocked: review.blocked,
        awaiting_flatten: review.awaiting_flatten,
        decision: review.decision,
        reason: review.reason,
        open_position_count: active_open_position_count(&dispatch_snapshot.open_positions),
        all_positions_broker_protected: all_open_positions_broker_protected(
            &dispatch_snapshot.open_positions,
        ),
    }
}

async fn refresh_market_data_view(
    state: &RuntimeHostState,
    sync_market_data_warmup: bool,
) -> RuntimeMarketDataView {
    let market_data_view = {
        let mut market_data = state.market_data.lock().await;
        market_data.refresh(Utc::now()).await
    };

    if sync_market_data_warmup {
        if let Some(snapshot) = market_data_view.snapshot.as_ref() {
            let mut operator = state.operator_state.lock().await;
            let _ = operator.sync_market_data_warmup(&snapshot.session.market_data.warmup);
        }
    }

    market_data_view
}

async fn market_data_refresh_loop(state: RuntimeHostState) {
    let mut interval = tokio::time::interval(MARKET_DATA_REFRESH_INTERVAL);
    loop {
        interval.tick().await;
        let _ = refresh_market_data_view(&state, true).await;
    }
}

async fn history_refresh_loop(state: RuntimeHostState) {
    let mut interval = tokio::time::interval(HISTORY_REFRESH_INTERVAL);
    loop {
        interval.tick().await;
        sync_history_state(&state).await;
    }
}

async fn health_refresh_loop(state: RuntimeHostState) {
    let mut interval = tokio::time::interval(HEALTH_REFRESH_INTERVAL);
    loop {
        let scheduled_at = interval.tick().await;
        let lag_ms = tokio::time::Instant::now()
            .saturating_duration_since(scheduled_at)
            .as_millis() as u64;
        let _ = state.health_supervisor.record_queue_lag(lag_ms);
        let dispatch_snapshot = current_dispatch_snapshot(&state).await;
        let market_data_view = refresh_market_data_view(&state, false).await;
        sync_system_health(&state, &dispatch_snapshot, &market_data_view).await;
    }
}

async fn current_dispatch_snapshot(state: &RuntimeHostState) -> RuntimeBrokerSnapshot {
    let handler = state.http_handler.lock().await;
    handler.dispatcher().snapshot()
}

async fn sync_history_state(state: &RuntimeHostState) {
    let snapshot = current_dispatch_snapshot(state).await;
    sync_history_snapshot(state, &snapshot).await;
}

async fn handle_runtime_shutdown_signal(state: &RuntimeHostState) -> bool {
    let snapshot = current_dispatch_snapshot(state).await;
    let open_position_count = active_open_position_count(&snapshot.open_positions);

    if open_position_count == 0 {
        approve_runtime_shutdown(
            state,
            RuntimeShutdownDecision::LeaveBrokerProtected,
            "shutdown approved after signal; no open broker position is active".to_owned(),
            ActionSource::System,
            json!({
                "decision": RuntimeShutdownDecision::LeaveBrokerProtected,
                "open_position_count": 0,
                "trigger": "signal",
            }),
        )
        .await;
        return false;
    }

    let message = if all_open_positions_broker_protected(&snapshot.open_positions) {
        format!(
            "shutdown blocked with {open_position_count} open broker-protected position(s); choose flatten first or explicitly leave broker-side protection in place"
        )
    } else {
        format!(
            "shutdown blocked with {open_position_count} open position(s); choose flatten first or explicitly accept leaving broker-side exposure in place"
        )
    };

    block_runtime_shutdown(state, message.clone(), true).await;
    journal_host_event(
        state,
        "runtime",
        "shutdown_blocked",
        ActionSource::System,
        EventSeverity::Warning,
        json!({
            "decision": RuntimeShutdownDecision::LeaveBrokerProtected,
            "open_position_count": open_position_count,
            "trigger": "signal",
            "reason": message,
            "all_positions_broker_protected": all_open_positions_broker_protected(
                &snapshot.open_positions
            ),
        }),
    )
    .await;
    warn!(
        open_position_count,
        "runtime shutdown blocked pending explicit review"
    );
    false
}

async fn finalize_pending_flatten_shutdown(state: &RuntimeHostState) -> bool {
    let pending = {
        let review = state.shutdown_review.lock().await;
        review.awaiting_flatten
    };
    if !pending {
        return false;
    }

    let snapshot = current_dispatch_snapshot(state).await;
    if active_open_position_count(&snapshot.open_positions) != 0 {
        return false;
    }

    {
        let mut review = state.shutdown_review.lock().await;
        review.pending_signal = false;
        review.blocked = false;
        review.awaiting_flatten = false;
        review.decision = Some(RuntimeShutdownDecision::FlattenFirst);
        review.reason =
            Some("shutdown approved after flatten confirmed no open positions".to_owned());
        review.requested_at = Some(Utc::now());
    }

    journal_host_event(
        state,
        "runtime",
        "shutdown_flatten_confirmed",
        ActionSource::System,
        EventSeverity::Info,
        json!({
            "decision": RuntimeShutdownDecision::FlattenFirst,
            "open_position_count": 0,
        }),
    )
    .await;

    true
}

async fn block_runtime_shutdown(state: &RuntimeHostState, reason: String, pending_signal: bool) {
    let mut review = state.shutdown_review.lock().await;
    review.pending_signal = pending_signal;
    review.blocked = true;
    review.awaiting_flatten = false;
    review.decision = None;
    review.reason = Some(reason);
    review.requested_at = Some(Utc::now());
}

async fn mark_shutdown_waiting_for_flatten(state: &RuntimeHostState, reason: String) {
    let mut review = state.shutdown_review.lock().await;
    review.pending_signal = false;
    review.blocked = true;
    review.awaiting_flatten = true;
    review.decision = Some(RuntimeShutdownDecision::FlattenFirst);
    review.reason = Some(reason);
    review.requested_at = Some(Utc::now());
}

async fn approve_runtime_shutdown(
    state: &RuntimeHostState,
    decision: RuntimeShutdownDecision,
    reason: String,
    source: ActionSource,
    payload: serde_json::Value,
) {
    {
        let mut review = state.shutdown_review.lock().await;
        review.pending_signal = false;
        review.blocked = false;
        review.awaiting_flatten = false;
        review.decision = Some(decision);
        review.reason = Some(reason.clone());
        review.requested_at = Some(Utc::now());
    }

    journal_host_event(
        state,
        "runtime",
        "shutdown_approved",
        source,
        EventSeverity::Warning,
        payload,
    )
    .await;

    let _ = state.shutdown_signal.send(true);
}

async fn sync_history_snapshot(state: &RuntimeHostState, snapshot: &RuntimeBrokerSnapshot) {
    let history_started_at = Instant::now();
    match state.history.sync_broker_snapshot(snapshot, Utc::now()) {
        Ok(Some(history_snapshot)) => {
            let _ = state
                .health_supervisor
                .record_db_write_latency(history_started_at.elapsed().as_millis() as u64);
            publish_history_snapshot(state, &history_snapshot).await
        }
        Ok(None) => {
            let _ = state
                .health_supervisor
                .record_db_write_latency(history_started_at.elapsed().as_millis() as u64);
        }
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            warn!(?error, "failed to sync runtime history snapshot");
        }
    }
}

async fn sync_system_health(
    state: &RuntimeHostState,
    dispatch_snapshot: &RuntimeBrokerSnapshot,
    market_data_view: &RuntimeMarketDataView,
) {
    let reconnect_count = dispatch_snapshot
        .broker_status
        .as_ref()
        .map(|snapshot| snapshot.reconnect_count)
        .unwrap_or(0)
        .saturating_add(
            market_data_view
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.session.market_data.reconnect_count)
                .unwrap_or(0),
        );
    let feed_degraded = market_data_view
        .snapshot
        .as_ref()
        .map(|snapshot| {
            !matches!(
                snapshot.session.market_data.health,
                tv_bot_market_data::MarketDataHealth::Healthy
            ) || !snapshot.trade_ready
        })
        .unwrap_or(false);
    let resource_sample = {
        let mut sampler = state.resource_sampler.lock().await;
        sampler.sample()
    };

    match state.health_supervisor.capture(
        RuntimeHealthInputs {
            cpu_percent: resource_sample.cpu_percent,
            memory_bytes: resource_sample.memory_bytes,
            reconnect_count,
            feed_degraded,
        },
        Utc::now(),
    ) {
        Ok(Some(snapshot)) => publish_system_health(state, &snapshot).await,
        Ok(None) => {}
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            warn!(?error, "failed to capture runtime system health");
        }
    }
}

fn scan_strategy_library(roots: Vec<PathBuf>) -> Result<RuntimeStrategyLibraryResponse, String> {
    let mut strategy_paths = Vec::new();

    for root in &roots {
        collect_strategy_paths(root, &mut strategy_paths)?;
    }

    strategy_paths.sort();

    let strategies = strategy_paths
        .into_iter()
        .map(validate_strategy_path)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|response| RuntimeStrategyCatalogEntry {
            path: response.path,
            display_path: response.display_path,
            valid: response.valid,
            title: response.title,
            strategy_id: response
                .summary
                .as_ref()
                .map(|summary| summary.strategy_id.clone()),
            name: response
                .summary
                .as_ref()
                .map(|summary| summary.name.clone()),
            version: response
                .summary
                .as_ref()
                .map(|summary| summary.version.clone()),
            market_family: response
                .summary
                .as_ref()
                .map(|summary| summary.market_family.clone()),
            warning_count: response.warnings.len(),
            error_count: response.errors.len(),
        })
        .collect();

    Ok(RuntimeStrategyLibraryResponse {
        scanned_roots: roots,
        strategies,
    })
}

fn collect_strategy_paths(root: &Path, strategy_paths: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|source| {
        format!(
            "failed to read strategy library root `{}`: {source}",
            root.display()
        )
    })? {
        let entry = entry.map_err(|source| {
            format!(
                "failed to read strategy library entry under `{}`: {source}",
                root.display()
            )
        })?;
        let path = entry.path();

        if path.is_dir() {
            collect_strategy_paths(&path, strategy_paths)?;
            continue;
        }

        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            strategy_paths.push(path);
        }
    }

    Ok(())
}

fn validate_strategy_path(path: PathBuf) -> Result<RuntimeStrategyValidationResponse, String> {
    let markdown = fs::read_to_string(&path).map_err(|source| {
        format!(
            "failed to read strategy file `{}`: {source}",
            path.display()
        )
    })?;
    let display_path = display_strategy_path(&path);
    let markdown_title = extract_markdown_title(&markdown);

    match StrictStrategyCompiler.compile_markdown(&markdown) {
        Ok(compilation) => {
            let summary = LoadedStrategySummary {
                path: path.clone(),
                title: compilation.title.clone(),
                strategy_id: compilation.compiled.metadata.strategy_id.clone(),
                name: compilation.compiled.metadata.name.clone(),
                version: compilation.compiled.metadata.version.clone(),
                market_family: compilation.compiled.market.market.clone(),
                warning_count: compilation.warnings.len(),
            };

            Ok(RuntimeStrategyValidationResponse {
                path,
                display_path,
                valid: true,
                title: compilation.title,
                summary: Some(summary),
                warnings: compilation
                    .warnings
                    .into_iter()
                    .map(control_api_strategy_issue)
                    .collect(),
                errors: Vec::new(),
            })
        }
        Err(error) => Ok(RuntimeStrategyValidationResponse {
            path,
            display_path,
            valid: false,
            title: markdown_title,
            summary: None,
            warnings: error
                .warnings
                .into_iter()
                .map(control_api_strategy_issue)
                .collect(),
            errors: error
                .errors
                .into_iter()
                .map(control_api_strategy_issue)
                .collect(),
        }),
    }
}

fn extract_markdown_title(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
}

fn control_api_strategy_issue(issue: StrategyIssue) -> RuntimeStrategyIssue {
    RuntimeStrategyIssue {
        severity: match issue.severity {
            StrategyIssueSeverity::Error => RuntimeStrategyIssueSeverity::Error,
            StrategyIssueSeverity::Warning => RuntimeStrategyIssueSeverity::Warning,
        },
        message: issue.message,
        section: issue.section,
        field: issue.field,
        line: issue.line,
    }
}

fn runtime_host_error_response(status: StatusCode, message: String) -> Response {
    (
        status,
        Json(HttpCommandResponse {
            status_code: HttpStatusCode::InternalServerError,
            body: HttpResponseBody::Error { message },
        }),
    )
        .into_response()
}

async fn runtime_lifecycle_success_response(
    state: &RuntimeHostState,
    response_status: HttpStatusCode,
    message: String,
    command_result: Option<tv_bot_control_api::ControlApiCommandResult>,
) -> Response {
    let context = status_context(state, false).await;
    let readiness = {
        let operator = state.operator_state.lock().await;
        operator.readiness_snapshot(&context)
    };
    publish_readiness_report(state, &readiness).await;

    let response = RuntimeLifecycleResponse {
        status_code: response_status,
        message,
        status: readiness.status.clone(),
        readiness,
        command_result,
    };

    (status_code(response.status_code), Json(response)).into_response()
}

async fn runtime_lifecycle_error_response(
    state: &RuntimeHostState,
    error: RuntimeOperatorError,
) -> Response {
    error!(?error, "runtime host lifecycle command failed");
    runtime_lifecycle_success_response(state, error.status_code(), error.to_string(), None).await
}

async fn publish_readiness_report(state: &RuntimeHostState, readiness: &RuntimeReadinessSnapshot) {
    if let Err(error) =
        state
            .event_hub
            .publish(tv_bot_control_api::ControlApiEvent::ReadinessReport {
                report: readiness.report.clone(),
                occurred_at: Utc::now(),
            })
    {
        if error != WebSocketEventHubError::NoSubscribers {
            warn!(?error, "failed to publish readiness report event");
        }
    }
}

fn publish_trade_latency(hub: &WebSocketEventHub, record: &TradePathLatencyRecord) {
    if let Err(error) = hub.publish(tv_bot_control_api::ControlApiEvent::TradeLatency {
        record: record.clone(),
        occurred_at: Utc::now(),
    }) {
        if error != WebSocketEventHubError::NoSubscribers {
            warn!(?error, "failed to publish trade latency event");
        }
    }
}

async fn publish_system_health(state: &RuntimeHostState, snapshot: &SystemHealthSnapshot) {
    if let Err(error) = state
        .event_hub
        .publish(tv_bot_control_api::ControlApiEvent::SystemHealth {
            snapshot: snapshot.clone(),
            occurred_at: Utc::now(),
        })
    {
        if error != WebSocketEventHubError::NoSubscribers {
            warn!(?error, "failed to publish system health event");
        }
    }
}

async fn publish_history_snapshot(state: &RuntimeHostState, snapshot: &RuntimeHistorySnapshot) {
    if let Err(error) =
        state
            .event_hub
            .publish(tv_bot_control_api::ControlApiEvent::HistorySnapshot {
                projection: snapshot.projection.clone(),
                occurred_at: Utc::now(),
            })
    {
        if error != WebSocketEventHubError::NoSubscribers {
            warn!(?error, "failed to publish history snapshot event");
        }
    }
}

async fn publish_journal_record(state: &RuntimeHostState, record: &EventJournalRecord) {
    if let Err(error) =
        state
            .event_hub
            .publish(tv_bot_control_api::ControlApiEvent::JournalRecord {
                record: record.clone(),
            })
    {
        if error != WebSocketEventHubError::NoSubscribers {
            warn!(?error, "failed to publish journal record event");
        }
    }
}

async fn journal_host_event(
    state: &RuntimeHostState,
    category: &str,
    action: &str,
    source: ActionSource,
    severity: EventSeverity,
    payload: serde_json::Value,
) {
    let occurred_at = Utc::now();
    let record = EventJournalRecord {
        event_id: host_event_id(category, action, occurred_at),
        category: category.to_owned(),
        action: action.to_owned(),
        source,
        severity,
        occurred_at,
        payload,
    };

    let started_at = Instant::now();
    let append_result = {
        let handler = state.http_handler.lock().await;
        handler.dispatcher().append_journal_record(record.clone())
    };

    match append_result {
        Ok(()) => {
            let _ = state
                .health_supervisor
                .record_db_write_latency(started_at.elapsed().as_millis() as u64);
            publish_journal_record(state, &record).await;
        }
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            warn!(
                ?error,
                category, action, "failed to persist runtime host journal event"
            );
        }
    }
}

fn request_for_command(
    command: &RuntimeCommand,
) -> &tv_bot_runtime_kernel::RuntimeExecutionRequest {
    match command {
        RuntimeCommand::ManualIntent(request) | RuntimeCommand::StrategyIntent(request) => request,
    }
}

fn runtime_latency_action_id(
    request: &tv_bot_runtime_kernel::RuntimeExecutionRequest,
    occurred_at: chrono::DateTime<Utc>,
) -> String {
    format!(
        "latency-{}-{}-{}",
        request.execution.strategy.metadata.strategy_id,
        action_source_label(request.action_source),
        occurred_at.timestamp_nanos_opt().unwrap_or_default()
    )
}

async fn sync_history_for_lifecycle_command(
    state: &RuntimeHostState,
    command: &RuntimeLifecycleCommand,
    source: tv_bot_control_api::ManualCommandSource,
) -> Result<(), RuntimeHistoryError> {
    let occurred_at = Utc::now();
    let maybe_snapshot = match command {
        RuntimeLifecycleCommand::SetMode { mode } => {
            state.history.record_mode_change(mode.clone())?
        }
        RuntimeLifecycleCommand::Arm { .. } => state
            .history
            .record_run_status(tv_bot_core_types::StrategyRunStatus::Active, occurred_at)?,
        RuntimeLifecycleCommand::Disarm => state
            .history
            .record_run_status(tv_bot_core_types::StrategyRunStatus::Starting, occurred_at)?,
        RuntimeLifecycleCommand::Pause => state
            .history
            .record_run_status(tv_bot_core_types::StrategyRunStatus::Paused, occurred_at)?,
        RuntimeLifecycleCommand::Resume => state
            .history
            .record_run_status(tv_bot_core_types::StrategyRunStatus::Active, occurred_at)?,
        RuntimeLifecycleCommand::MarkWarmupFailed { .. } => state
            .history
            .record_run_status(tv_bot_core_types::StrategyRunStatus::Failed, occurred_at)?,
        RuntimeLifecycleCommand::LoadStrategy { .. }
        | RuntimeLifecycleCommand::StartWarmup
        | RuntimeLifecycleCommand::MarkWarmupReady
        | RuntimeLifecycleCommand::ResolveReconnectReview { .. }
        | RuntimeLifecycleCommand::Shutdown { .. }
        | RuntimeLifecycleCommand::Flatten { .. } => None,
    };

    if let Some(snapshot) = maybe_snapshot {
        publish_history_snapshot(state, &snapshot).await;
    }

    let _ = source;
    sync_history_state(state).await;
    Ok(())
}

fn parse_fallback_addr(bind: &str) -> SocketAddr {
    bind.parse()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0)))
}

fn host_health_status(
    state: &RuntimeHostState,
    system_health: Option<&SystemHealthSnapshot>,
) -> String {
    if !state.command_dispatch_ready {
        return "degraded".to_owned();
    }

    if !state.storage_status.durable || !state.journal_status.durable {
        return "degraded".to_owned();
    }

    match system_health {
        Some(snapshot) if snapshot.feed_degraded || snapshot.error_count > 0 => {
            "degraded".to_owned()
        }
        Some(_) => "ok".to_owned(),
        None => "initializing".to_owned(),
    }
}

fn action_source_label(source: tv_bot_core_types::ActionSource) -> &'static str {
    match source {
        tv_bot_core_types::ActionSource::Dashboard => "dashboard",
        tv_bot_core_types::ActionSource::Cli => "cli",
        tv_bot_core_types::ActionSource::System => "system",
    }
}

fn host_event_id(category: &str, action: &str, occurred_at: DateTime<Utc>) -> String {
    let timestamp = occurred_at.timestamp_nanos_opt().unwrap_or_default();
    format!("{category}-{action}-{timestamp}")
}

fn active_open_position_count(positions: &[tv_bot_core_types::BrokerPositionSnapshot]) -> usize {
    positions
        .iter()
        .filter(|position| position.quantity != 0)
        .count()
}

fn all_open_positions_broker_protected(
    positions: &[tv_bot_core_types::BrokerPositionSnapshot],
) -> bool {
    let open_positions = positions
        .iter()
        .filter(|position| position.quantity != 0)
        .collect::<Vec<_>>();

    !open_positions.is_empty()
        && open_positions
            .iter()
            .all(|position| position.protective_orders_present)
}

fn reconnect_decision_label(decision: RuntimeReconnectDecision) -> &'static str {
    match decision {
        RuntimeReconnectDecision::ClosePosition => "close_position",
        RuntimeReconnectDecision::LeaveBrokerProtected => "leave_broker_protected",
        RuntimeReconnectDecision::ReattachBotManagement => "reattach_bot_management",
    }
}

fn runtime_reconnect_decision_from_tradovate(
    decision: tv_bot_broker_tradovate::TradovateReconnectDecision,
) -> RuntimeReconnectDecision {
    match decision {
        tv_bot_broker_tradovate::TradovateReconnectDecision::ClosePosition => {
            RuntimeReconnectDecision::ClosePosition
        }
        tv_bot_broker_tradovate::TradovateReconnectDecision::LeaveBrokerProtected => {
            RuntimeReconnectDecision::LeaveBrokerProtected
        }
        tv_bot_broker_tradovate::TradovateReconnectDecision::ReattachBotManagement => {
            RuntimeReconnectDecision::ReattachBotManagement
        }
    }
}

fn tradovate_reconnect_decision(
    decision: RuntimeReconnectDecision,
) -> tv_bot_broker_tradovate::TradovateReconnectDecision {
    match decision {
        RuntimeReconnectDecision::ClosePosition => {
            tv_bot_broker_tradovate::TradovateReconnectDecision::ClosePosition
        }
        RuntimeReconnectDecision::LeaveBrokerProtected => {
            tv_bot_broker_tradovate::TradovateReconnectDecision::LeaveBrokerProtected
        }
        RuntimeReconnectDecision::ReattachBotManagement => {
            tv_bot_broker_tradovate::TradovateReconnectDecision::ReattachBotManagement
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        path::PathBuf,
        sync::{Arc, Mutex as StdMutex},
        time::{SystemTime, UNIX_EPOCH},
    };

    use async_trait::async_trait;
    use axum::{body::Body, http::Request, response::Response, Router};
    use http_body_util::BodyExt;
    use rust_decimal::Decimal;
    use secrecy::SecretString;
    use tower::ServiceExt;
    use tv_bot_broker_tradovate::{
        TradovateAccessToken, TradovateAccount, TradovateAccountApi, TradovateAccountListRequest,
        TradovateAuthApi, TradovateAuthRequest, TradovateCredentials, TradovateError,
        TradovateExecutionApi, TradovateLiquidatePositionRequest, TradovateLiquidatePositionResult,
        TradovatePlaceOrderRequest, TradovatePlaceOrderResult, TradovatePlaceOsoRequest,
        TradovatePlaceOsoResult, TradovateReconnectDecision, TradovateRoutingPreferences,
        TradovateSessionConfig, TradovateSessionManager, TradovateSyncApi,
        TradovateSyncConnectRequest, TradovateSyncEvent, TradovateSyncSnapshot,
        TradovateUserSyncRequest,
    };
    use tv_bot_config::{AppConfig, MapEnvironment};
    use tv_bot_control_api::{
        ControlApiCommandResult, ControlApiCommandStatus, ManualCommandSource,
    };
    use tv_bot_core_types::{
        ActionSource, ArmState, BreakEvenRule, BrokerOrderUpdate, BrokerPositionSnapshot,
        BrokerPreference, CompiledStrategy, ContractMode, DailyLossLimit, DashboardDisplay,
        DataFeedRequirement, DataRequirements, EntryOrderType, EntryRules, ExecutionIntent,
        ExecutionSpec, ExitRules, FailsafeRules, FeedType, MarketConfig, MarketSelection,
        PartialTakeProfitRule, PositionSizing, PositionSizingMode, ReversalMode, RiskDecision,
        RiskDecisionStatus, RiskLimits, ScalingConfig, SessionMode, SessionRules,
        SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata, Timeframe,
        TradeManagement, TrailingRule, WarmupStatus,
    };
    use tv_bot_execution_engine::{
        ExecutionDispatchReport, ExecutionDispatchResult, ExecutionInstrumentContext,
        ExecutionRequest, ExecutionStateContext,
    };
    use tv_bot_journal::{EventJournal, InMemoryJournal};
    use tv_bot_persistence::RuntimePersistence;
    use tv_bot_risk_engine::{BrokerProtectionSupport, RiskInstrumentContext, RiskStateContext};
    use tv_bot_runtime_kernel::{RuntimeExecutionOutcome, RuntimeExecutionRequest};

    use super::*;

    struct FakeDispatcher {
        result: Option<Result<RuntimeCommandOutcome, RuntimeCommandError>>,
        snapshot: RuntimeBrokerSnapshot,
        dispatched_commands: std::sync::Arc<std::sync::Mutex<Vec<RuntimeCommand>>>,
    }

    fn empty_dispatch_log() -> std::sync::Arc<std::sync::Mutex<Vec<RuntimeCommand>>> {
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    async fn request_with_timeout(app: Router, request: Request<Body>, label: &str) -> Response {
        tokio::time::timeout(tokio::time::Duration::from_secs(5), app.oneshot(request))
            .await
            .unwrap_or_else(|_| panic!("{label} timed out"))
            .expect("router should respond")
    }

    #[derive(Default)]
    struct FixedResourceSampler {
        sample: RuntimeResourceSample,
    }

    #[derive(Clone)]
    struct TestAuthApi {
        token: Arc<StdMutex<Option<TradovateAccessToken>>>,
    }

    #[derive(Clone)]
    struct TestAccountApi {
        accounts: Arc<Vec<TradovateAccount>>,
    }

    #[derive(Clone)]
    struct TestSyncApi {
        snapshots: Arc<StdMutex<VecDeque<TradovateSyncSnapshot>>>,
        events: Arc<StdMutex<VecDeque<TradovateSyncEvent>>>,
    }

    #[derive(Clone, Debug, Default)]
    struct TestExecutionApi {
        place_orders: Arc<StdMutex<Vec<TradovatePlaceOrderRequest>>>,
        place_osos: Arc<StdMutex<Vec<TradovatePlaceOsoRequest>>>,
        liquidations: Arc<StdMutex<Vec<TradovateLiquidatePositionRequest>>>,
    }

    type TestKernelDispatcher = RuntimeKernelCommandDispatcher<
        TestAuthApi,
        TestAccountApi,
        TestSyncApi,
        tv_bot_broker_tradovate::SystemClock,
        TestExecutionApi,
        InMemoryJournal,
    >;

    struct KernelBackedDispatcher {
        inner: TestKernelDispatcher,
    }

    impl RuntimeResourceSampler for FixedResourceSampler {
        fn sample(&mut self) -> RuntimeResourceSample {
            self.sample.clone()
        }
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
            Ok(self
                .events
                .lock()
                .expect("sync mutex should not poison")
                .pop_front())
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
            Ok(TradovatePlaceOrderResult { order_id: 8101 })
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
                order_id: 8102,
                oso1_id: Some(8103),
                oso2_id: Some(8104),
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
            Ok(TradovateLiquidatePositionResult { order_id: 8105 })
        }
    }

    #[async_trait]
    impl RuntimeCommandDispatcher for FakeDispatcher {
        async fn dispatch(
            &mut self,
            command: RuntimeCommand,
        ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
            self.dispatched_commands
                .lock()
                .expect("dispatch log mutex should not poison")
                .push(command);
            self.result
                .take()
                .expect("fake dispatcher should have a queued result")
        }
    }

    impl RuntimeDispatcherHandle for FakeDispatcher {
        fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot {
            self.snapshot.clone()
        }

        fn append_journal_record(&self, _record: EventJournalRecord) -> Result<(), JournalError> {
            Ok(())
        }

        fn acknowledge_reconnect_review(
            &mut self,
            decision: RuntimeReconnectDecision,
        ) -> Result<(), String> {
            self.snapshot.last_reconnect_review_decision = Some(decision);
            if let Some(status) = self.snapshot.broker_status.as_mut() {
                status.sync_state = tv_bot_core_types::BrokerSyncState::Synchronized;
                status.review_required_reason = None;
            }
            Ok(())
        }
    }

    #[async_trait]
    impl RuntimeCommandDispatcher for KernelBackedDispatcher {
        async fn dispatch(
            &mut self,
            command: RuntimeCommand,
        ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
            self.inner.dispatch(command).await
        }
    }

    impl RuntimeDispatcherHandle for KernelBackedDispatcher {
        fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot {
            let session = self.inner.session().snapshot();

            RuntimeBrokerSnapshot {
                broker_status: Some(session.broker),
                last_reconnect_review_decision: session
                    .last_review_decision
                    .map(runtime_reconnect_decision_from_tradovate),
                account_snapshot: session.account_snapshot,
                open_positions: session.open_positions,
                working_orders: session.working_orders,
                fills: session.fills,
            }
        }

        fn append_journal_record(&self, record: EventJournalRecord) -> Result<(), JournalError> {
            self.inner.journal().append(record)
        }

        fn acknowledge_reconnect_review(
            &mut self,
            decision: RuntimeReconnectDecision,
        ) -> Result<(), String> {
            self.inner
                .session_mut()
                .acknowledge_reconnect_review(tradovate_reconnect_decision(decision));
            Ok(())
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

    fn sync_snapshot_with_open_position() -> TradovateSyncSnapshot {
        let snapshot = sample_dispatch_snapshot();

        TradovateSyncSnapshot {
            occurred_at: Utc::now(),
            positions: snapshot.open_positions,
            working_orders: snapshot.working_orders,
            fills: snapshot.fills,
            account_snapshot: snapshot.account_snapshot,
            mismatch_reason: None,
            detail: "synced".to_owned(),
        }
    }

    fn sync_snapshot_without_open_exposure() -> TradovateSyncSnapshot {
        let mut snapshot = sample_dispatch_snapshot();
        snapshot.open_positions.clear();
        snapshot.working_orders.clear();
        snapshot.fills.clear();

        TradovateSyncSnapshot {
            occurred_at: Utc::now(),
            positions: snapshot.open_positions,
            working_orders: snapshot.working_orders,
            fills: snapshot.fills,
            account_snapshot: snapshot.account_snapshot,
            mismatch_reason: None,
            detail: "synced clean".to_owned(),
        }
    }

    async fn sample_session_manager_with_open_position(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        let auth_api = TestAuthApi {
            token: Arc::new(StdMutex::new(Some(sample_token()))),
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
            snapshots: Arc::new(StdMutex::new(VecDeque::from([
                sync_snapshot_with_open_position(),
            ]))),
            events: Arc::new(StdMutex::new(VecDeque::new())),
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
        manager.acknowledge_reconnect_review(TradovateReconnectDecision::LeaveBrokerProtected);

        manager
    }

    async fn sample_session_manager_with_reconnect_review_required(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        let auth_api = TestAuthApi {
            token: Arc::new(StdMutex::new(Some(sample_token()))),
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
            snapshots: Arc::new(StdMutex::new(VecDeque::from([
                sync_snapshot_without_open_exposure(),
                sync_snapshot_with_open_position(),
            ]))),
            events: Arc::new(StdMutex::new(VecDeque::from([
                TradovateSyncEvent::Disconnected {
                    occurred_at: Utc::now(),
                    reason: "test reconnect".to_owned(),
                },
                TradovateSyncEvent::Reconnected {
                    occurred_at: Utc::now(),
                    detail: "test reconnect completed".to_owned(),
                },
            ]))),
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
            .expect("initial sync should connect");
        let disconnect_event = manager
            .poll_next_event()
            .await
            .expect("disconnect event should poll");
        assert!(matches!(
            disconnect_event,
            Some(TradovateSyncEvent::Disconnected { .. })
        ));
        let reconnect_event = manager
            .poll_next_event()
            .await
            .expect("reconnect event should poll");
        assert!(matches!(
            reconnect_event,
            Some(TradovateSyncEvent::Reconnected { .. })
        ));
        manager
            .reconnect_user_sync()
            .await
            .expect("reconnect sync should connect");

        manager
    }

    fn test_history() -> RuntimeHistoryRecorder {
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
        .expect("test config should load");
        let persistence = RuntimePersistence::open(&config);

        RuntimeHistoryRecorder::from_persistence(&persistence)
            .expect("history recorder should initialize")
    }

    fn test_latency_collector() -> Arc<RuntimeLatencyCollector> {
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
        .expect("test config should load");
        let persistence = RuntimePersistence::open(&config);

        Arc::new(
            RuntimeLatencyCollector::from_persistence(&persistence)
                .expect("latency collector should initialize"),
        )
    }

    fn test_health_supervisor() -> Arc<RuntimeHealthSupervisor> {
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
        .expect("test config should load");
        let persistence = RuntimePersistence::open(&config);

        Arc::new(
            RuntimeHealthSupervisor::from_persistence(&persistence)
                .expect("health supervisor should initialize"),
        )
    }

    fn test_state(
        dispatcher: BoxedDispatcher,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
    ) -> RuntimeHostState {
        test_state_with_strategy_roots(
            dispatcher,
            history,
            latency_collector,
            health_supervisor,
            Vec::new(),
        )
    }

    fn test_state_with_strategy_roots(
        dispatcher: BoxedDispatcher,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
        strategy_library_roots: Vec<PathBuf>,
    ) -> RuntimeHostState {
        let event_hub = WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build");
        let handler = HttpCommandHandler::with_publisher(
            LocalControlApi::new(dispatcher),
            BestEffortEventPublisher {
                hub: event_hub.clone(),
            },
        );
        let operator_state =
            RuntimeOperatorState::new(RuntimeStateMachine::new(RuntimeMode::Observation));

        RuntimeHostState {
            http_handler: Arc::new(Mutex::new(handler)),
            history,
            latency_collector,
            health_supervisor,
            resource_sampler: Arc::new(Mutex::new(Box::new(FixedResourceSampler {
                sample: RuntimeResourceSample {
                    cpu_percent: Some(18.5),
                    memory_bytes: Some(12_582_912),
                },
            }))),
            market_data: Arc::new(Mutex::new(RuntimeMarketDataManager {
                config: None,
                state: RuntimeMarketDataState::PendingStrategy {
                    detail: "load a strategy to prepare the Databento market-data service"
                        .to_owned(),
                },
            })),
            event_hub,
            operator_state: Arc::new(Mutex::new(operator_state)),
            http_bind: "127.0.0.1:8080".to_owned(),
            websocket_bind: "127.0.0.1:8081".to_owned(),
            command_dispatch_ready: true,
            command_dispatch_detail: "ready".to_owned(),
            storage_status: RuntimeStorageStatus {
                mode: RuntimeStorageMode::PrimaryConfigured,
                primary_configured: true,
                sqlite_fallback_enabled: false,
                sqlite_path: std::path::PathBuf::from("data/tv_bot_core.sqlite"),
                allow_runtime_fallback: false,
                active_backend: "postgres".to_owned(),
                durable: true,
                fallback_activated: false,
                detail: "primary Postgres persistence is active".to_owned(),
            },
            journal_status: RuntimeJournalStatus {
                backend: "postgres".to_owned(),
                durable: true,
                detail: "event journal records are durably persisted to Postgres".to_owned(),
            },
            strategy_library_roots,
            shutdown_signal: watch::channel(false).0,
            shutdown_review: Arc::new(Mutex::new(ShutdownReviewState::default())),
        }
    }

    fn build_kernel_backed_state_with_manager(
        manager: TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi>,
        execution_api: TestExecutionApi,
        journal: InMemoryJournal,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
    ) -> RuntimeHostState {
        let event_hub = WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build");
        let dispatcher = BoxedDispatcher::new(
            Box::new(KernelBackedDispatcher {
                inner: RuntimeKernelCommandDispatcher::new(manager, execution_api, journal),
            }),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
            event_hub,
        );

        test_state(dispatcher, history, latency_collector, health_supervisor)
    }

    async fn build_kernel_backed_state(
        execution_api: TestExecutionApi,
        journal: InMemoryJournal,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
    ) -> RuntimeHostState {
        build_kernel_backed_state_with_manager(
            sample_session_manager_with_open_position().await,
            execution_api,
            journal,
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
    }

    fn sample_request() -> HttpCommandRequest {
        HttpCommandRequest {
            command: ControlApiCommand::ManualIntent {
                source: tv_bot_control_api::ManualCommandSource::Cli,
                request: RuntimeExecutionRequest {
                    mode: RuntimeMode::Paper,
                    action_source: ActionSource::Cli,
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
                            reason: "host-test".to_owned(),
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
                        broker_support: BrokerProtectionSupport {
                            stop_loss: true,
                            take_profit: true,
                            trailing_stop: true,
                            daily_loss_limit: true,
                        },
                        hard_override_active: false,
                    },
                },
            },
        }
    }

    fn sample_market_data_snapshot(
        health: tv_bot_market_data::MarketDataHealth,
    ) -> MarketDataServiceSnapshot {
        let now = chrono::Utc::now();
        let trade_feed_state = if matches!(health, tv_bot_market_data::MarketDataHealth::Degraded) {
            tv_bot_market_data::FeedReadinessState::Degraded
        } else {
            tv_bot_market_data::FeedReadinessState::Ready
        };
        let trade_feed_detail = if matches!(health, tv_bot_market_data::MarketDataHealth::Degraded)
        {
            "trade feed degraded"
        } else {
            "trade feed ready"
        };

        MarketDataServiceSnapshot {
            session: tv_bot_market_data::DatabentoSessionStatus {
                market_data: tv_bot_market_data::MarketDataStatusSnapshot {
                    provider: "databento".to_owned(),
                    dataset: "GLBX.MDP3".to_owned(),
                    connection_state: tv_bot_market_data::MarketDataConnectionState::Subscribed,
                    health,
                    feed_statuses: vec![
                        tv_bot_market_data::FeedStatus {
                            instrument_symbol: "GCM2026".to_owned(),
                            feed: tv_bot_core_types::FeedType::Trades,
                            state: trade_feed_state,
                            last_event_at: Some(now),
                            detail: trade_feed_detail.to_owned(),
                        },
                        tv_bot_market_data::FeedStatus {
                            instrument_symbol: "GCM2026".to_owned(),
                            feed: tv_bot_core_types::FeedType::Ohlcv1m,
                            state: tv_bot_market_data::FeedReadinessState::Ready,
                            last_event_at: Some(now),
                            detail: "bar feed ready".to_owned(),
                        },
                    ],
                    warmup: tv_bot_market_data::WarmupProgress {
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
                        started_at: Some(now),
                        updated_at: now,
                        failure_reason: None,
                    },
                    reconnect_count: 0,
                    last_heartbeat_at: Some(now),
                    last_disconnect_reason: None,
                    updated_at: now,
                },
            },
            warmup_requested: true,
            warmup_mode: tv_bot_market_data::DatabentoWarmupMode::LiveOnly,
            replay_caught_up: true,
            trade_ready: matches!(health, tv_bot_market_data::MarketDataHealth::Healthy),
            updated_at: now,
        }
    }

    async fn set_test_market_data_snapshot(
        state: &RuntimeHostState,
        snapshot: MarketDataServiceSnapshot,
        detail: Option<String>,
    ) {
        let mut market_data = state.market_data.lock().await;
        market_data.state = RuntimeMarketDataState::SnapshotOverride { snapshot, detail };
    }

    fn sample_strategy() -> CompiledStrategy {
        CompiledStrategy {
            metadata: StrategyMetadata {
                schema_version: 1,
                strategy_id: "gc_runtime_host_v1".to_owned(),
                name: "GC Runtime Host".to_owned(),
                version: "1.0.0".to_owned(),
                author: "tests".to_owned(),
                description: "runtime host tests".to_owned(),
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
                bars_required: std::collections::BTreeMap::from([(Timeframe::OneMinute, 10)]),
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

    fn sample_outcome() -> RuntimeCommandOutcome {
        RuntimeCommandOutcome::Execution(RuntimeExecutionOutcome {
            risk: tv_bot_risk_engine::RiskEvaluationOutcome {
                decision: RiskDecision {
                    status: RiskDecisionStatus::Accepted,
                    reason: "risk checks passed".to_owned(),
                    warnings: vec!["warning-1".to_owned()],
                },
                adjusted_intent: ExecutionIntent::PauseStrategy {
                    reason: "pause".to_owned(),
                },
                approved_quantity: None,
                hard_override_reasons: Vec::new(),
            },
            dispatch: None,
        })
    }

    fn sample_dispatched_outcome() -> RuntimeCommandOutcome {
        RuntimeCommandOutcome::Execution(RuntimeExecutionOutcome {
            risk: tv_bot_risk_engine::RiskEvaluationOutcome {
                decision: RiskDecision {
                    status: RiskDecisionStatus::Accepted,
                    reason: "risk checks passed".to_owned(),
                    warnings: vec!["warning-1".to_owned()],
                },
                adjusted_intent: ExecutionIntent::Enter {
                    side: tv_bot_core_types::TradeSide::Buy,
                    order_type: EntryOrderType::Market,
                    quantity: 1,
                    protective_brackets_expected: false,
                    reason: "dispatch".to_owned(),
                },
                approved_quantity: Some(1),
                hard_override_reasons: Vec::new(),
            },
            dispatch: Some(ExecutionDispatchReport {
                results: vec![ExecutionDispatchResult::OrderSubmitted {
                    order_id: 42,
                    symbol: "GCM2026".to_owned(),
                    used_brackets: false,
                }],
                warnings: Vec::new(),
            }),
        })
    }

    fn sample_dispatch_snapshot() -> RuntimeBrokerSnapshot {
        RuntimeBrokerSnapshot {
            broker_status: Some(BrokerStatusSnapshot {
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
            }),
            account_snapshot: Some(tv_bot_core_types::BrokerAccountSnapshot {
                account_id: "101".to_owned(),
                account_name: Some("paper-primary".to_owned()),
                cash_balance: Some(Decimal::new(50_000, 0)),
                available_funds: Some(Decimal::new(49_850, 0)),
                excess_liquidity: Some(Decimal::new(49_850, 0)),
                margin_used: Some(Decimal::new(150, 0)),
                net_liquidation_value: Some(Decimal::new(50_125, 0)),
                realized_pnl: Some(Decimal::new(125, 0)),
                unrealized_pnl: Some(Decimal::new(-75, 0)),
                risk_state: Some("healthy".to_owned()),
                captured_at: chrono::Utc::now(),
            }),
            open_positions: vec![BrokerPositionSnapshot {
                account_id: Some("101".to_owned()),
                symbol: "GCM2026".to_owned(),
                quantity: 1,
                average_price: Some(Decimal::new(238_500, 2)),
                realized_pnl: None,
                unrealized_pnl: Some(Decimal::new(-75, 0)),
                protective_orders_present: true,
                captured_at: chrono::Utc::now(),
            }],
            working_orders: vec![BrokerOrderUpdate {
                broker_order_id: "ord-1".to_owned(),
                account_id: Some("101".to_owned()),
                symbol: "GCM2026".to_owned(),
                side: Some(tv_bot_core_types::TradeSide::Buy),
                quantity: Some(1),
                order_type: Some(EntryOrderType::Limit),
                status: tv_bot_core_types::BrokerOrderStatus::Working,
                filled_quantity: 0,
                average_fill_price: None,
                updated_at: chrono::Utc::now(),
            }],
            fills: vec![tv_bot_core_types::BrokerFillUpdate {
                fill_id: "fill-1".to_owned(),
                broker_order_id: Some("ord-1".to_owned()),
                account_id: Some("101".to_owned()),
                symbol: "GCM2026".to_owned(),
                side: tv_bot_core_types::TradeSide::Buy,
                quantity: 1,
                price: Decimal::new(238_500, 2),
                fee: Some(Decimal::new(125, 2)),
                commission: Some(Decimal::new(75, 2)),
                occurred_at: chrono::Utc::now(),
            }],
            last_reconnect_review_decision: None,
        }
    }

    fn temp_strategy_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("tv_bot_runtime_host_{unique}.md"))
    }

    fn temp_strategy_library_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("tv_bot_runtime_host_library_{unique}"))
    }

    fn temp_sqlite_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("tv_bot_runtime_host_{unique}.sqlite"))
    }

    fn write_strategy_file(path: &PathBuf) {
        fs::write(
            path,
            include_str!("../../../strategies/examples/gc_momentum_fade_v1.md"),
        )
        .expect("strategy file should write");
    }

    fn write_invalid_strategy_file(path: &PathBuf) {
        fs::write(
            path,
            r#"# Broken Strategy

## Metadata
schema_version: 1
strategy_id: broken_strategy_v1
name: Broken Strategy
version: "1.0.0"
author: tests
description: broken validation fixture
tags: []

## Market
market: gold
selection:
  contract_mode: front_month_auto
"#,
        )
        .expect("invalid strategy file should write");
    }

    #[tokio::test]
    async fn status_route_reports_runtime_state() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Err(RuntimeCommandError::Execution {
                        source: tv_bot_runtime_kernel::RuntimeExecutionError::Dispatch {
                            source: tv_bot_execution_engine::ExecutionDispatchError::Planning {
                                source:
                                    tv_bot_execution_engine::ExecutionEngineError::NewEntriesBlocked,
                            },
                        },
                    })),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status: RuntimeStatusSnapshot =
            serde_json::from_slice(&body).expect("status json should parse");

        assert_eq!(status.mode, RuntimeMode::Observation);
        assert_eq!(status.arm_state, ArmState::Disarmed);
        assert_eq!(status.warmup_status, WarmupStatus::NotLoaded);
        assert!(status.command_dispatch_ready);
        assert_eq!(
            status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(status.broker_status.is_some());
        assert_eq!(
            status
                .system_health
                .as_ref()
                .and_then(|snapshot| snapshot.cpu_percent),
            Some(18.5)
        );
        assert_eq!(
            status
                .system_health
                .as_ref()
                .and_then(|snapshot| snapshot.memory_bytes),
            Some(12_582_912)
        );
    }

    #[tokio::test]
    async fn commands_route_succeeds_without_websocket_subscribers() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::LoadStrategy {
                            path: strategy_path.clone(),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "load strategy request",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&sample_request()).expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let command_response: HttpCommandResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(command_response.status_code, HttpStatusCode::Ok);
        match command_response.body {
            tv_bot_control_api::HttpResponseBody::CommandResult(result) => {
                assert_eq!(result.status, ControlApiCommandStatus::Executed);
                assert_eq!(
                    result,
                    ControlApiCommandResult {
                        status: ControlApiCommandStatus::Executed,
                        risk_status: RiskDecisionStatus::Accepted,
                        dispatch_performed: false,
                        reason: "risk checks passed".to_owned(),
                        warnings: vec!["warning-1".to_owned()],
                    }
                );
            }
            other => panic!("unexpected body: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn strategies_route_lists_valid_and_invalid_strategy_files() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let strategy_root = temp_strategy_library_root();
        fs::create_dir_all(&strategy_root).expect("strategy root should create");

        let valid_path = strategy_root.join("gc_valid.md");
        let invalid_path = strategy_root.join("broken.md");
        write_strategy_file(&valid_path);
        write_invalid_strategy_file(&invalid_path);

        let app = build_http_router(test_state_with_strategy_roots(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
            vec![strategy_root.clone()],
        ));

        let response = request_with_timeout(
            app,
            Request::builder()
                .uri("/strategies")
                .body(Body::empty())
                .expect("request should build"),
            "strategy library request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let library: RuntimeStrategyLibraryResponse =
            serde_json::from_slice(&body).expect("library json should parse");

        assert_eq!(library.scanned_roots, vec![strategy_root.clone()]);
        assert_eq!(library.strategies.len(), 2);

        let valid_entry = library
            .strategies
            .iter()
            .find(|entry| entry.path == valid_path)
            .expect("valid entry should be present");
        assert!(valid_entry.valid);
        assert_eq!(
            valid_entry.strategy_id.as_deref(),
            Some("gc_momentum_fade_v1")
        );
        assert_eq!(valid_entry.name.as_deref(), Some("GC Momentum Fade"));
        assert_eq!(valid_entry.error_count, 0);

        let invalid_entry = library
            .strategies
            .iter()
            .find(|entry| entry.path == invalid_path)
            .expect("invalid entry should be present");
        assert!(!invalid_entry.valid);
        assert_eq!(invalid_entry.title.as_deref(), Some("Broken Strategy"));
        assert!(invalid_entry.error_count > 0);
        assert!(invalid_entry.strategy_id.is_none());

        let _ = fs::remove_dir_all(strategy_root);
    }

    #[tokio::test]
    async fn strategy_validation_route_returns_compile_issues_and_journals_result() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state(
            execution_api,
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
        .await;
        let app = build_http_router(state);
        let strategy_path = temp_strategy_path();
        write_invalid_strategy_file(&strategy_path);

        let response = request_with_timeout(
            app,
            Request::builder()
                .method("POST")
                .uri("/strategies/validate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeStrategyValidationRequest {
                        source: ManualCommandSource::Dashboard,
                        path: strategy_path.clone(),
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "strategy validation request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let validation: RuntimeStrategyValidationResponse =
            serde_json::from_slice(&body).expect("validation json should parse");

        assert_eq!(validation.path, strategy_path);
        assert_eq!(validation.title.as_deref(), Some("Broken Strategy"));
        assert!(!validation.valid);
        assert!(validation.summary.is_none());
        assert!(!validation.errors.is_empty());
        assert!(validation
            .errors
            .iter()
            .all(|issue| issue.severity == RuntimeStrategyIssueSeverity::Error));

        let journal_records = journal.list().expect("journal should list records");
        let validation_record = journal_records
            .iter()
            .find(|record| record.action == "validation_failed")
            .expect("validation result should be journaled");
        assert_eq!(validation_record.category, "strategy");
        assert_eq!(validation_record.source, ActionSource::Dashboard);
        assert_eq!(
            validation_record.payload["path"].as_str(),
            Some(validation.display_path.as_str())
        );
        assert_eq!(validation_record.payload["valid"].as_bool(), Some(false));
        assert_eq!(
            validation_record.payload["error_count"].as_u64(),
            Some(validation.errors.len() as u64)
        );

        let _ = fs::remove_file(validation.path);
    }

    #[tokio::test]
    async fn paper_flatten_command_dispatches_to_paper_account_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state(
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
        .await;
        let app = build_http_router(state);
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        for command in [
            RuntimeLifecycleCommand::LoadStrategy {
                path: strategy_path.clone(),
            },
            RuntimeLifecycleCommand::SetMode {
                mode: RuntimeMode::Paper,
            },
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/runtime/commands")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&RuntimeLifecycleRequest {
                                source: ManualCommandSource::Cli,
                                command,
                            })
                            .expect("request should serialize"),
                        ))
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(response.status(), StatusCode::OK);
        }

        let history_before = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(history_before.status(), StatusCode::OK);
        let history_before_body = history_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_before: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_before_body).expect("history json should parse");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::Flatten {
                                contract_id: 4444,
                                reason: "manual paper flatten".to_owned(),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(lifecycle_response.status_code, HttpStatusCode::Ok);
        assert_eq!(lifecycle_response.message, "flatten command dispatched");
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(
            lifecycle_response.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        let command_result = lifecycle_response
            .command_result
            .expect("flatten should return a command result");
        assert_eq!(command_result.status, ControlApiCommandStatus::Executed);
        assert_eq!(command_result.risk_status, RiskDecisionStatus::Accepted);
        assert!(command_result.dispatch_performed);

        let place_orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_orders.is_empty());
        drop(place_orders);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_osos.is_empty());
        drop(place_osos);

        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(liquidations.len(), 1);
        let liquidation = &liquidations[0];
        assert_eq!(liquidation.context.account_id, 101);
        assert_eq!(liquidation.context.account_spec, "paper-primary");
        assert_eq!(liquidation.contract_id, 4444);
        drop(liquidations);

        let history_after = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(history_after.status(), StatusCode::OK);
        let history_after_body = history_after
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_body).expect("history json should parse");
        assert!(
            history_after.projection.total_order_records
                > history_before.projection.total_order_records
        );
        assert!(history_after.projection.latest_order.is_some());

        let journal_actions = journal
            .list()
            .expect("journal should list records")
            .into_iter()
            .map(|record| record.action)
            .collect::<Vec<_>>();
        assert_eq!(
            journal_actions,
            vec![
                "intent_received".to_owned(),
                "decision".to_owned(),
                "dispatch_succeeded".to_owned(),
            ]
        );

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn arm_command_returns_precondition_required_when_override_is_missing() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state(
            execution_api,
            journal,
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
        .await;
        let app = build_http_router(state.clone());
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::LoadStrategy {
                            path: strategy_path.clone(),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "load strategy request",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
        )
        .await;

        for (label, command) in [
            (
                "set mode paper request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "mark warmup ready request",
                RuntimeLifecycleCommand::MarkWarmupReady,
            ),
        ] {
            let response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command,
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                label,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::Arm {
                            allow_override: false,
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "arm without override request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");
        assert_eq!(
            lifecycle_response.status_code,
            HttpStatusCode::PreconditionRequired
        );
        assert!(lifecycle_response
            .message
            .contains("hard override is required"));
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(lifecycle_response.status.arm_state, ArmState::Disarmed);

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_entry_command_is_blocked_when_market_data_is_degraded_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state(
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
        .await;
        let app = build_http_router(state.clone());
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::LoadStrategy {
                            path: strategy_path.clone(),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "load strategy request",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
        )
        .await;

        for (label, command) in [
            (
                "set mode paper request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "mark warmup ready request",
                RuntimeLifecycleCommand::MarkWarmupReady,
            ),
            (
                "arm runtime request",
                RuntimeLifecycleCommand::Arm {
                    allow_override: true,
                },
            ),
        ] {
            let response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command,
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                label,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let status_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "healthy status request",
        )
        .await;
        assert_eq!(status_before.status(), StatusCode::OK);
        let status_before_body = status_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status_before: RuntimeStatusSnapshot =
            serde_json::from_slice(&status_before_body).expect("status json should parse");
        assert_eq!(status_before.mode, RuntimeMode::Paper);
        assert_eq!(status_before.arm_state, ArmState::Armed);
        assert_eq!(
            status_before
                .market_data_status
                .as_ref()
                .map(|snapshot| snapshot.session.market_data.health),
            Some(tv_bot_market_data::MarketDataHealth::Healthy)
        );

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "history before request",
        )
        .await;
        assert_eq!(history_before.status(), StatusCode::OK);
        let history_before_body = history_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_before: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_before_body).expect("history json should parse");

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Degraded),
            Some("market data is degraded; new entries must remain blocked".to_owned()),
        )
        .await;

        let status_degraded = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "degraded status request",
        )
        .await;
        assert_eq!(status_degraded.status(), StatusCode::OK);
        let status_degraded_body = status_degraded
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status_degraded: RuntimeStatusSnapshot =
            serde_json::from_slice(&status_degraded_body).expect("status json should parse");
        assert_eq!(status_degraded.mode, RuntimeMode::Paper);
        assert_eq!(status_degraded.arm_state, ArmState::Armed);
        assert_eq!(
            status_degraded.market_data_detail.as_deref(),
            Some("market data is degraded; new entries must remain blocked")
        );
        assert_eq!(
            status_degraded
                .market_data_status
                .as_ref()
                .map(|snapshot| snapshot.session.market_data.health),
            Some(tv_bot_market_data::MarketDataHealth::Degraded)
        );

        let response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&sample_request()).expect("request should serialize"),
                ))
                .expect("request should build"),
            "degraded entry command request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let command_response: HttpCommandResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(command_response.status_code, HttpStatusCode::Conflict);
        match command_response.body {
            HttpResponseBody::Error { message } => {
                assert!(message.contains("new entries are blocked"));
            }
            other => panic!("unexpected body: {other:?}"),
        }

        let place_orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_orders.is_empty());
        drop(place_orders);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_osos.is_empty());
        drop(place_osos);

        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert!(liquidations.is_empty());
        drop(liquidations);

        let history_after = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "history after request",
        )
        .await;
        assert_eq!(history_after.status(), StatusCode::OK);
        let history_after_body = history_after
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_body).expect("history json should parse");
        assert_eq!(
            history_after.projection.total_order_records,
            history_before.projection.total_order_records
        );

        let journal_records = journal.list().expect("journal should list records");
        let journal_actions = journal_records
            .iter()
            .map(|record| record.action.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            journal_actions,
            vec![
                "intent_received".to_owned(),
                "decision".to_owned(),
                "dispatch_failed".to_owned(),
            ]
        );
        assert!(journal_records[2].payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("new entries are blocked")));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_scale_in_command_dispatches_broker_side_brackets_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state(
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
        .await;
        let app = build_http_router(state.clone());
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::LoadStrategy {
                            path: strategy_path.clone(),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "load strategy request",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
        )
        .await;

        for (label, command) in [
            (
                "set mode paper request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "mark warmup ready request",
                RuntimeLifecycleCommand::MarkWarmupReady,
            ),
            (
                "arm runtime request",
                RuntimeLifecycleCommand::Arm {
                    allow_override: true,
                },
            ),
        ] {
            let response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command,
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                label,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let status_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "paper scale-in status request",
        )
        .await;
        assert_eq!(status_before.status(), StatusCode::OK);
        let status_before_body = status_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status_before: RuntimeStatusSnapshot =
            serde_json::from_slice(&status_before_body).expect("status json should parse");
        assert_eq!(status_before.mode, RuntimeMode::Paper);
        assert_eq!(status_before.arm_state, ArmState::Armed);
        assert_eq!(
            status_before.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert_eq!(
            status_before
                .market_data_status
                .as_ref()
                .map(|snapshot| snapshot.session.market_data.health),
            Some(tv_bot_market_data::MarketDataHealth::Healthy)
        );

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "history before scale-in request",
        )
        .await;
        assert_eq!(history_before.status(), StatusCode::OK);
        let history_before_body = history_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_before: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_before_body).expect("history json should parse");

        let mut request = sample_request();
        match &mut request.command {
            ControlApiCommand::ManualIntent { request, .. }
            | ControlApiCommand::StrategyIntent { request } => {
                request.execution.intent = ExecutionIntent::Enter {
                    side: tv_bot_core_types::TradeSide::Buy,
                    order_type: EntryOrderType::Market,
                    quantity: 1,
                    protective_brackets_expected: true,
                    reason: "manual paper scale in".to_owned(),
                };
            }
        }

        let response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&request).expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper scale-in command request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let command_response: HttpCommandResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(command_response.status_code, HttpStatusCode::Ok);
        match command_response.body {
            HttpResponseBody::CommandResult(result) => {
                assert_eq!(result.status, ControlApiCommandStatus::Executed);
                assert_eq!(result.risk_status, RiskDecisionStatus::Accepted);
                assert!(result.dispatch_performed);
            }
            other => panic!("unexpected body: {other:?}"),
        }

        let place_orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_orders.is_empty());
        drop(place_orders);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(place_osos.len(), 1);
        let oso = &place_osos[0];
        assert_eq!(oso.context.account_id, 101);
        assert_eq!(oso.context.account_spec, "paper-primary");
        assert_eq!(oso.order.symbol, "GCM2026");
        assert_eq!(oso.order.quantity, 1);
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
        drop(place_osos);

        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert!(liquidations.is_empty());
        drop(liquidations);

        let history_after = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "history after scale-in request",
        )
        .await;
        assert_eq!(history_after.status(), StatusCode::OK);
        let history_after_body = history_after
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_body).expect("history json should parse");
        assert!(
            history_after.projection.total_order_records
                > history_before.projection.total_order_records
        );
        assert!(history_after.projection.latest_order.is_some());

        let journal_actions = journal
            .list()
            .expect("journal should list records")
            .into_iter()
            .map(|record| record.action)
            .collect::<Vec<_>>();
        assert_eq!(
            journal_actions,
            vec![
                "intent_received".to_owned(),
                "decision".to_owned(),
                "dispatch_succeeded".to_owned(),
            ]
        );

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn history_route_projects_broker_snapshot_state() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let snapshot: RuntimeHistorySnapshot =
            serde_json::from_slice(&body).expect("history json should parse");

        assert_eq!(snapshot.projection.total_order_records, 1);
        assert_eq!(snapshot.projection.total_fill_records, 1);
        assert_eq!(snapshot.projection.total_position_records, 1);
        assert_eq!(snapshot.projection.open_trade_ids.len(), 1);
        assert_eq!(
            snapshot
                .projection
                .latest_position
                .as_ref()
                .map(|record| record.symbol.as_str()),
            Some("GCM2026")
        );
    }

    #[tokio::test]
    async fn health_route_reports_system_health_and_latest_trade_latency() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_dispatched_outcome())),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::LoadStrategy {
                                path: strategy_path.clone(),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&sample_request()).expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let health: RuntimeHostHealthResponse =
            serde_json::from_slice(&body).expect("health json should parse");

        assert_eq!(health.status, "ok");
        assert_eq!(
            health
                .system_health
                .as_ref()
                .and_then(|snapshot| snapshot.cpu_percent),
            Some(18.5)
        );
        assert_eq!(
            health
                .system_health
                .as_ref()
                .and_then(|snapshot| snapshot.memory_bytes),
            Some(12_582_912)
        );
        assert!(health.latest_trade_latency.is_some());

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn commands_route_returns_conflict_when_new_entries_are_blocked_server_side() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Err(RuntimeCommandError::Execution {
                        source: tv_bot_runtime_kernel::RuntimeExecutionError::Dispatch {
                            source: tv_bot_execution_engine::ExecutionDispatchError::Planning {
                                source:
                                    tv_bot_execution_engine::ExecutionEngineError::NewEntriesBlocked,
                            },
                        },
                    })),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::LoadStrategy {
                                path: strategy_path.clone(),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(load_response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&sample_request()).expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let command_response: HttpCommandResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(command_response.status_code, HttpStatusCode::Conflict);
        match command_response.body {
            HttpResponseBody::Error { message } => {
                assert!(message.contains("new entries are blocked"));
            }
            other => panic!("unexpected body: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn reconnect_review_command_resolves_required_review() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let mut snapshot = sample_dispatch_snapshot();
        if let Some(status) = snapshot.broker_status.as_mut() {
            status.sync_state = tv_bot_core_types::BrokerSyncState::ReviewRequired;
            status.review_required_reason =
                Some("active broker position detected after reconnect".to_owned());
        }
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot,
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::ResolveReconnectReview {
                                decision: RuntimeReconnectDecision::LeaveBrokerProtected,
                                contract_id: None,
                                reason: Some("keeping broker protections live".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(lifecycle_response.status_code, HttpStatusCode::Ok);
        assert!(!lifecycle_response.status.reconnect_review.required);
        assert_eq!(
            lifecycle_response.status.reconnect_review.last_decision,
            Some(RuntimeReconnectDecision::LeaveBrokerProtected)
        );
    }

    #[tokio::test]
    async fn paper_reconnect_review_after_disconnect_can_reattach_bot_management_through_runtime_host(
    ) {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_reconnect_review_required().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
        let app = build_http_router(state);

        let set_mode_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::SetMode {
                            mode: RuntimeMode::Paper,
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "set mode paper request",
        )
        .await;
        assert_eq!(set_mode_response.status(), StatusCode::OK);

        let status_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "paper reconnect status request",
        )
        .await;
        assert_eq!(status_before.status(), StatusCode::OK);
        let status_before_body = status_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status_before: RuntimeStatusSnapshot =
            serde_json::from_slice(&status_before_body).expect("status json should parse");
        assert_eq!(status_before.mode, RuntimeMode::Paper);
        assert_eq!(
            status_before.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(status_before.reconnect_review.required);
        assert_eq!(
            status_before.reconnect_review.reason.as_deref(),
            Some("existing broker-side position or working orders detected after reconnect")
        );
        assert_eq!(status_before.reconnect_review.last_decision, None);
        assert_eq!(status_before.reconnect_review.open_position_count, 1);
        assert_eq!(status_before.reconnect_review.working_order_count, 1);
        assert_eq!(
            status_before
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.sync_state),
            Some(tv_bot_core_types::BrokerSyncState::ReviewRequired)
        );
        assert_eq!(
            status_before
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.reconnect_count),
            Some(1)
        );

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper reconnect history before request",
        )
        .await;
        assert_eq!(history_before.status(), StatusCode::OK);
        let history_before_body = history_before
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_before: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_before_body).expect("history json should parse");

        let response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::ResolveReconnectReview {
                            decision: RuntimeReconnectDecision::ReattachBotManagement,
                            contract_id: None,
                            reason: Some("resume paper bot management after reconnect".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper reconnect reattach request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(lifecycle_response.status_code, HttpStatusCode::Ok);
        assert_eq!(
            lifecycle_response.message,
            "reconnect review resolved with reattach_bot_management"
        );
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(
            lifecycle_response.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(!lifecycle_response.status.reconnect_review.required);
        assert_eq!(
            lifecycle_response.status.reconnect_review.last_decision,
            Some(RuntimeReconnectDecision::ReattachBotManagement)
        );
        assert_eq!(
            lifecycle_response
                .status
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.sync_state),
            Some(tv_bot_core_types::BrokerSyncState::Synchronized)
        );
        assert_eq!(
            lifecycle_response
                .status
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.reconnect_count),
            Some(1)
        );

        let place_orders = execution_api
            .place_orders
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_orders.is_empty());
        drop(place_orders);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert!(place_osos.is_empty());
        drop(place_osos);

        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert!(liquidations.is_empty());
        drop(liquidations);

        let history_after = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper reconnect history after request",
        )
        .await;
        assert_eq!(history_after.status(), StatusCode::OK);
        let history_after_body = history_after
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_body).expect("history json should parse");
        assert!(
            history_after.projection.total_position_records
                >= history_before.projection.total_position_records
        );
        assert!(history_after.projection.total_position_records > 0);
        assert_eq!(
            history_after
                .projection
                .latest_position
                .as_ref()
                .map(|record| record.symbol.as_str()),
            Some("GCM2026")
        );

        let journal_records = journal.list().expect("journal should list records");
        let reconnect_resolution = journal_records
            .iter()
            .find(|record| record.action == "reconnect_review_resolved")
            .expect("reconnect resolution should be journaled");
        assert_eq!(reconnect_resolution.category, "broker");
        assert_eq!(
            reconnect_resolution.payload["reason"].as_str(),
            Some("resume paper bot management after reconnect")
        );
        assert_eq!(
            reconnect_resolution.payload["open_position_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            reconnect_resolution.payload["working_order_count"].as_u64(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn reconnect_review_close_position_dispatches_flatten_request() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let dispatched_commands = empty_dispatch_log();
        let mut snapshot = sample_dispatch_snapshot();
        if let Some(status) = snapshot.broker_status.as_mut() {
            status.sync_state = tv_bot_core_types::BrokerSyncState::ReviewRequired;
            status.review_required_reason =
                Some("active broker position detected after reconnect".to_owned());
        }
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot,
                    dispatched_commands: dispatched_commands.clone(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        for command in [
            RuntimeLifecycleCommand::LoadStrategy {
                path: strategy_path.clone(),
            },
            RuntimeLifecycleCommand::SetMode {
                mode: RuntimeMode::Paper,
            },
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/runtime/commands")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&RuntimeLifecycleRequest {
                                source: ManualCommandSource::Cli,
                                command,
                            })
                            .expect("request should serialize"),
                        ))
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::ResolveReconnectReview {
                                decision: RuntimeReconnectDecision::ClosePosition,
                                contract_id: Some(4444),
                                reason: Some("close during reconnect review".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(lifecycle_response.status_code, HttpStatusCode::Ok);

        let dispatched_commands = dispatched_commands
            .lock()
            .expect("dispatch log mutex should not poison");
        assert_eq!(dispatched_commands.len(), 1);
        match &dispatched_commands[0] {
            RuntimeCommand::ManualIntent(request) => {
                assert_eq!(request.mode, RuntimeMode::Paper);
                assert_eq!(request.action_source, ActionSource::Cli);
                assert_eq!(request.execution.instrument.active_contract_id, Some(4444));
                assert!(request.execution.state.current_position.is_some());
                assert_eq!(
                    request.execution.intent,
                    ExecutionIntent::Flatten {
                        reason: "close during reconnect review".to_owned(),
                    }
                );
            }
            other => panic!("unexpected runtime command: {other:?}"),
        }

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn shutdown_command_blocks_when_open_position_lacks_broker_protection() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let mut snapshot = sample_dispatch_snapshot();
        snapshot.open_positions[0].protective_orders_present = false;
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot,
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::Shutdown {
                                decision: RuntimeShutdownDecision::LeaveBrokerProtected,
                                contract_id: None,
                                reason: Some("testing shutdown safety".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(lifecycle_response.status_code, HttpStatusCode::Conflict);
        assert!(lifecycle_response.status.shutdown_review.blocked);
        assert!(
            !lifecycle_response
                .status
                .shutdown_review
                .all_positions_broker_protected
        );
    }

    #[tokio::test]
    async fn shutdown_flatten_first_marks_shutdown_waiting_for_flatten_confirmation() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        ));
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::LoadStrategy {
                                path: strategy_path.clone(),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(load_response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::Shutdown {
                                decision: RuntimeShutdownDecision::FlattenFirst,
                                contract_id: Some(4444),
                                reason: Some("shutdown after flatten".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let lifecycle_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&body).expect("response json should parse");

        assert_eq!(lifecycle_response.status_code, HttpStatusCode::Ok);
        assert!(lifecycle_response.status.shutdown_review.blocked);
        assert!(lifecycle_response.status.shutdown_review.awaiting_flatten);

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn shutdown_signal_blocks_until_operator_reviews_open_position() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let state = test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot: sample_dispatch_snapshot(),
                    dispatched_commands: empty_dispatch_log(),
                }),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
                WebSocketEventHub::new(EVENT_HUB_CAPACITY).expect("hub should build"),
            ),
            history,
            latency_collector,
            health_supervisor,
        );

        let should_stop = handle_runtime_shutdown_signal(&state).await;

        assert!(!should_stop);

        let review = state.shutdown_review.lock().await.clone();
        assert!(review.pending_signal);
        assert!(review.blocked);
        assert!(!review.awaiting_flatten);
        assert!(review
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("choose flatten first")));
    }

    #[test]
    fn build_runtime_host_state_starts_with_dispatch_unavailable_when_broker_config_is_missing() {
        let config = AppConfig::from_toml_str(
            "runtime.example.toml",
            r#"
                [runtime]
                startup_mode = "observation"

                [control_api]
                http_bind = "127.0.0.1:18080"
                websocket_bind = "127.0.0.1:18081"
            "#,
            &MapEnvironment::default(),
        )
        .expect("config should load");

        let state =
            build_runtime_host_state(&config, RuntimeStateMachine::new(RuntimeMode::Observation))
                .expect("runtime host state should build");

        assert!(!state.command_dispatch_ready);
        assert!(state
            .command_dispatch_detail
            .contains("missing broker configuration"));
    }

    #[test]
    fn build_runtime_host_state_uses_sqlite_journal_when_configured() {
        let sqlite_path = temp_sqlite_path();
        let escaped_sqlite_path = sqlite_path.display().to_string().replace('\\', "\\\\");
        let config = AppConfig::from_toml_str(
            "runtime.example.toml",
            &format!(
                r#"
                [runtime]
                startup_mode = "paper"
                allow_sqlite_fallback = true

                [persistence.sqlite_fallback]
                enabled = true
                path = "{}"

                [control_api]
                http_bind = "127.0.0.1:18082"
                websocket_bind = "127.0.0.1:18083"
            "#,
                escaped_sqlite_path
            ),
            &MapEnvironment::default(),
        )
        .expect("config should load");

        let state = build_runtime_host_state(&config, RuntimeStateMachine::new(RuntimeMode::Paper))
            .expect("runtime host state should build");

        assert_eq!(state.storage_status.active_backend, "sqlite");
        assert!(state.storage_status.durable);
        assert_eq!(state.journal_status.backend, "sqlite");
        assert!(state.journal_status.durable);

        let _ = fs::remove_file(sqlite_path);
    }
}
