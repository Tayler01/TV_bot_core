use std::{net::SocketAddr, sync::Arc, time::Instant};

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
use chrono::Utc;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::Mutex,
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
    HttpCommandResponse, HttpResponseBody, HttpStatusCode, LocalControlApi,
    RuntimeCommandDispatcher, RuntimeHistorySnapshot, RuntimeJournalStatus,
    RuntimeKernelCommandDispatcher, RuntimeLifecycleCommand, RuntimeLifecycleRequest,
    RuntimeLifecycleResponse, RuntimeReadinessSnapshot, RuntimeStatusSnapshot, RuntimeStorageMode,
    RuntimeStorageStatus, WebSocketEventHub, WebSocketEventHubError, WebSocketEventStreamError,
};
use tv_bot_core_types::{
    BrokerStatusSnapshot, RuntimeMode, SystemHealthSnapshot, TradePathLatencyRecord,
    TradePathTimestamps,
};
use tv_bot_health::{RuntimeHealthError, RuntimeHealthInputs, RuntimeHealthSupervisor};
use tv_bot_journal::{JournalError, PersistentJournal, ProjectingJournal};
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
}

struct RuntimeMarketDataManager {
    config: Option<RuntimeMarketDataConfig>,
    state: RuntimeMarketDataState,
}

trait RuntimeDispatcherHandle: RuntimeCommandDispatcher {
    fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot;
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
            account_snapshot: session.account_snapshot,
            open_positions: session.open_positions,
            working_orders: session.working_orders,
            fills: session.fills,
        }
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
        }
    }
}

#[derive(Clone)]
pub struct RuntimeHostState {
    http_handler: Arc<Mutex<HttpCommandHandler<BoxedDispatcher, BestEffortEventPublisher>>>,
    history: RuntimeHistoryRecorder,
    latency_collector: Arc<RuntimeLatencyCollector>,
    health_supervisor: Arc<RuntimeHealthSupervisor>,
    market_data: Arc<Mutex<RuntimeMarketDataManager>>,
    event_hub: WebSocketEventHub,
    operator_state: Arc<Mutex<RuntimeOperatorState>>,
    http_bind: String,
    websocket_bind: String,
    command_dispatch_ready: bool,
    command_dispatch_detail: String,
    storage_status: RuntimeStorageStatus,
    journal_status: RuntimeJournalStatus,
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

    tokio::signal::ctrl_c()
        .await
        .map_err(|source| RuntimeHostError::ShutdownSignal { source })?;
    info!("shutdown signal received");

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
    let event_hub = WebSocketEventHub::new(EVENT_HUB_CAPACITY)
        .map_err(|source| RuntimeHostError::EventHub { source })?;
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
    })
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
    let message = {
        let mut operator = state.operator_state.lock().await;
        match operator.apply_lifecycle_command(command, &context) {
            Ok(message) => message,
            Err(error) => {
                return runtime_lifecycle_error_response(&state, error).await;
            }
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
    let (message, market_data_seed, current_mode) = {
        let mut operator = state.operator_state.lock().await;
        let message = match operator
            .apply_lifecycle_command(RuntimeLifecycleCommand::LoadStrategy { path }, &context)
        {
            Ok(message) => message,
            Err(error) => return runtime_lifecycle_error_response(&state, error).await,
        };
        let market_data_seed = operator.market_data_seed().ok();
        let current_mode = operator.status_snapshot(&context).mode;
        (message, market_data_seed, current_mode)
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
    let message = {
        let mut operator = state.operator_state.lock().await;
        match operator.apply_lifecycle_command(RuntimeLifecycleCommand::StartWarmup, &context) {
            Ok(message) => message,
            Err(error) => return runtime_lifecycle_error_response(&state, error).await,
        }
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
    let sanitized_request = match request_result {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    let mut handler = state.http_handler.lock().await;
    match handler.handle_command(sanitized_request).await {
        Ok(response) => {
            drop(handler);
            sync_history_state(&state).await;
            let command_result = match response.body {
                HttpResponseBody::CommandResult(result) => Some(result),
                HttpResponseBody::Error { message } => {
                    return runtime_lifecycle_success_response(
                        &state,
                        response.status_code,
                        message,
                        None,
                    )
                    .await;
                }
            };

            runtime_lifecycle_success_response(
                &state,
                response.status_code,
                "flatten command dispatched".to_owned(),
                command_result,
            )
            .await
        }
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            error!(?error, "runtime host flatten command failed");
            runtime_lifecycle_success_response(
                &state,
                HttpStatusCode::InternalServerError,
                error.to_string(),
                None,
            )
            .await
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
        interval.tick().await;
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

    match state.health_supervisor.capture(
        RuntimeHealthInputs {
            cpu_percent: None,
            memory_bytes: None,
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use rust_decimal::Decimal;
    use tower::ServiceExt;
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
    use tv_bot_persistence::RuntimePersistence;
    use tv_bot_risk_engine::{BrokerProtectionSupport, RiskInstrumentContext, RiskStateContext};
    use tv_bot_runtime_kernel::{RuntimeExecutionOutcome, RuntimeExecutionRequest};

    use super::*;

    struct FakeDispatcher {
        result: Option<Result<RuntimeCommandOutcome, RuntimeCommandError>>,
        snapshot: RuntimeBrokerSnapshot,
    }

    #[async_trait]
    impl RuntimeCommandDispatcher for FakeDispatcher {
        async fn dispatch(
            &mut self,
            _command: RuntimeCommand,
        ) -> Result<RuntimeCommandOutcome, RuntimeCommandError> {
            self.result
                .take()
                .expect("fake dispatcher should have a queued result")
        }
    }

    impl RuntimeDispatcherHandle for FakeDispatcher {
        fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot {
            self.snapshot.clone()
        }
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
        }
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
        }
    }

    fn temp_strategy_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("tv_bot_runtime_host_{unique}.md"))
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
        assert!(status.system_health.is_some());
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
    async fn history_route_projects_broker_snapshot_state() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let app = build_http_router(test_state(
            BoxedDispatcher::new(
                Box::new(FakeDispatcher {
                    result: Some(Ok(sample_outcome())),
                    snapshot: sample_dispatch_snapshot(),
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
        assert!(health.system_health.is_some());
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
