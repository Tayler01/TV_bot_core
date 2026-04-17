use std::{
    collections::{BTreeMap, BTreeSet},
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
        Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
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
use tv_bot_config::{
    persist_runtime_settings_update, AppConfig, ConfigUpdateError, RuntimeSettingsFileUpdate,
};
#[cfg(test)]
use tv_bot_control_api::ControlApiCommand;
use tv_bot_control_api::{
    ControlApiEventPublisher, HttpCommandHandler, HttpCommandRequest, HttpCommandResponse,
    HttpResponseBody, HttpStatusCode, LoadedStrategySummary, LocalControlApi, RuntimeChartBar,
    RuntimeChartConfigResponse, RuntimeChartHistoryResponse, RuntimeChartInstrumentSummary,
    RuntimeChartSnapshot, RuntimeChartStreamEvent, RuntimeCommandDispatcher,
    RuntimeEditableSettings, RuntimeHistorySnapshot, RuntimeJournalSnapshot, RuntimeJournalStatus,
    RuntimeKernelCommandDispatcher, RuntimeLifecycleCommand, RuntimeLifecycleRequest,
    RuntimeLifecycleResponse, RuntimeReadinessSnapshot, RuntimeReconnectDecision,
    RuntimeReconnectReviewStatus, RuntimeSettingsPersistenceMode, RuntimeSettingsSnapshot,
    RuntimeSettingsUpdateRequest, RuntimeSettingsUpdateResponse, RuntimeShutdownDecision,
    RuntimeShutdownReviewStatus, RuntimeStatusSnapshot, RuntimeStorageMode, RuntimeStorageStatus,
    RuntimeStrategyCatalogEntry, RuntimeStrategyIssue, RuntimeStrategyIssueSeverity,
    RuntimeStrategyLibraryResponse, RuntimeStrategyUploadRequest, RuntimeStrategyValidationRequest,
    RuntimeStrategyValidationResponse, WebSocketEventHub, WebSocketEventHubError,
    WebSocketEventStreamError,
};
use tv_bot_core_types::{
    ActionSource, EventJournalRecord, EventSeverity, MarketEvent, SystemHealthSnapshot, Timeframe,
    TradePathLatencyRecord, TradePathTimestamps,
};
#[cfg(test)]
use tv_bot_core_types::{BrokerStatusSnapshot, RuntimeMode};
use tv_bot_health::{
    RuntimeHealthError, RuntimeHealthInputs, RuntimeHealthSupervisor, RuntimeResourceSampler,
    SysinfoRuntimeResourceSampler,
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
use tv_bot_strategy_loader::{StrategyIssue, StrategyIssueSeverity, StrictStrategyCompiler};

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
const JOURNAL_ROUTE_LIMIT: usize = 50;
const CHART_DEFAULT_LIMIT: usize = 300;
const CHART_MAX_LIMIT: usize = 1_000;
const CHART_RECENT_FILL_LIMIT: usize = 20;
const CHART_STREAM_INTERVAL: Duration = Duration::from_millis(250);
const SAMPLE_CHART_BAR_COUNT: usize = 480;
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
    sample_chart_active: bool,
}

enum RuntimeMarketDataState {
    Unconfigured {
        detail: String,
        chart_bars: BTreeMap<Timeframe, Vec<RuntimeChartBar>>,
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
        warmup_mode: DatabentoWarmupMode,
    },
    #[cfg(test)]
    SnapshotOverride {
        snapshot: MarketDataServiceSnapshot,
        detail: Option<String>,
        chart_bars: BTreeMap<Timeframe, Vec<RuntimeChartBar>>,
    },
}

struct RuntimeMarketDataManager {
    config: Option<RuntimeMarketDataConfig>,
    state: RuntimeMarketDataState,
}

fn strategy_warmup_mode(
    strategy: &tv_bot_core_types::CompiledStrategy,
    now: DateTime<Utc>,
) -> DatabentoWarmupMode {
    strategy_warmup_replay_from(strategy, now)
        .map(DatabentoWarmupMode::ReplayFrom)
        .unwrap_or(DatabentoWarmupMode::LiveOnly)
}

fn strategy_warmup_replay_from(
    strategy: &tv_bot_core_types::CompiledStrategy,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    strategy
        .warmup
        .bars_required
        .iter()
        .filter_map(|(timeframe, required_bars)| {
            let required_bars = i32::try_from(*required_bars).ok()?;
            let replay_window = timeframe_duration(*timeframe).checked_mul(required_bars)?;
            Some(align_timeframe_start(now, *timeframe) - replay_window)
        })
        .min()
}

fn timeframe_duration(timeframe: tv_bot_core_types::Timeframe) -> ChronoDuration {
    match timeframe {
        tv_bot_core_types::Timeframe::OneSecond => ChronoDuration::seconds(1),
        tv_bot_core_types::Timeframe::OneMinute => ChronoDuration::minutes(1),
        tv_bot_core_types::Timeframe::FiveMinute => ChronoDuration::minutes(5),
    }
}

fn align_timeframe_start(
    timestamp: DateTime<Utc>,
    timeframe: tv_bot_core_types::Timeframe,
) -> DateTime<Utc> {
    let seconds = timeframe_duration(timeframe).num_seconds();
    let aligned_seconds = timestamp.timestamp() - timestamp.timestamp().rem_euclid(seconds);

    DateTime::<Utc>::from_timestamp(aligned_seconds, 0)
        .expect("aligned warmup timestamp should be valid")
}

fn sample_chart_bars_for_strategy(
    seed: &LoadedStrategyMarketDataSeed,
    now: DateTime<Utc>,
) -> BTreeMap<Timeframe, Vec<RuntimeChartBar>> {
    let supported_timeframes = supported_chart_timeframes(&seed.strategy);
    let price_seed = seed
        .instrument_mapping
        .as_ref()
        .map(|mapping| mapping.tradovate_symbol.as_str())
        .unwrap_or(seed.strategy.metadata.strategy_id.as_str());
    let base_price_cents = sample_chart_base_price_cents(price_seed);

    supported_timeframes
        .into_iter()
        .map(|timeframe| {
            (
                timeframe,
                sample_chart_bars_for_timeframe(timeframe, now, base_price_cents),
            )
        })
        .collect()
}

fn sample_chart_base_price_cents(seed: &str) -> i64 {
    let fingerprint = seed.bytes().fold(0u64, |accumulator, byte| {
        accumulator.wrapping_mul(131).wrapping_add(u64::from(byte))
    });
    let whole_units = 900 + i64::try_from(fingerprint % 3_200).unwrap_or(0);
    whole_units * 100
}

fn sample_chart_bars_for_timeframe(
    timeframe: Timeframe,
    now: DateTime<Utc>,
    base_price_cents: i64,
) -> Vec<RuntimeChartBar> {
    let (drift_per_bar, swing_size, wick_size, volume_base) = match timeframe {
        Timeframe::OneSecond => (1_i64, 12_i64, 6_i64, 18_u64),
        Timeframe::OneMinute => (4_i64, 28_i64, 12_i64, 80_u64),
        Timeframe::FiveMinute => (9_i64, 54_i64, 22_i64, 220_u64),
    };
    let timeframe_step = timeframe_duration(timeframe);
    let anchor = align_timeframe_start(now, timeframe);
    let first_bar_time = anchor
        - timeframe_step
            .checked_mul(i32::try_from(SAMPLE_CHART_BAR_COUNT.saturating_sub(1)).unwrap_or(0))
            .unwrap_or_else(ChronoDuration::zero);
    let midpoint = i64::try_from(SAMPLE_CHART_BAR_COUNT / 2).unwrap_or(0);
    let mut previous_close = base_price_cents - (swing_size / 2);

    (0..SAMPLE_CHART_BAR_COUNT)
        .map(|index| {
            let index_i64 = i64::try_from(index).unwrap_or(0);
            let trend = (index_i64 - midpoint) * drift_per_bar;
            let primary_wave = ((index_i64 % 14) - 7) * swing_size;
            let secondary_wave = ((index_i64 % 5) - 2) * (swing_size / 2);
            let close = base_price_cents + trend + primary_wave + secondary_wave;
            let open = previous_close;
            let high = open.max(close) + wick_size + (index_i64 % 4) * 2;
            let low = open.min(close) - wick_size - (index_i64 % 3) * 2;
            let volume = volume_base
                + u64::try_from((index_i64 % 9) * 11).unwrap_or(0)
                + u64::try_from((index_i64 % 4) * 7).unwrap_or(0);
            let closed_at = first_bar_time
                + timeframe_step
                    .checked_mul(i32::try_from(index).unwrap_or(0))
                    .unwrap_or_else(ChronoDuration::zero);

            previous_close = close;

            RuntimeChartBar {
                timeframe,
                open: Decimal::new(open, 2),
                high: Decimal::new(high, 2),
                low: Decimal::new(low, 2),
                close: Decimal::new(close, 2),
                volume,
                closed_at,
            }
        })
        .collect()
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

#[derive(Clone, Debug)]
struct RuntimeSettingsState {
    editable: RuntimeEditableSettings,
    http_bind: String,
    websocket_bind: String,
    config_file_path: Option<PathBuf>,
}

impl RuntimeSettingsState {
    fn from_config(config: &AppConfig, config_file_path: Option<PathBuf>) -> Self {
        Self {
            editable: RuntimeEditableSettings {
                startup_mode: config.runtime.startup_mode.clone(),
                default_strategy_path: config.runtime.default_strategy_path.clone(),
                allow_sqlite_fallback: config.runtime.allow_sqlite_fallback,
                paper_account_name: config.broker.paper_account_name.clone(),
                live_account_name: config.broker.live_account_name.clone(),
            },
            http_bind: config.control_api.http_bind.clone(),
            websocket_bind: config.control_api.websocket_bind.clone(),
            config_file_path,
        }
    }

    fn snapshot(&self) -> RuntimeSettingsSnapshot {
        let persistence_mode = if self.config_file_path.is_some() {
            RuntimeSettingsPersistenceMode::ConfigFile
        } else {
            RuntimeSettingsPersistenceMode::SessionOnly
        };
        let detail = match persistence_mode {
            RuntimeSettingsPersistenceMode::ConfigFile => {
                "settings edits are saved to the runtime config file for the next restart; environment overrides may still take precedence".to_owned()
            }
            RuntimeSettingsPersistenceMode::SessionOnly => {
                "runtime launched without a config file path; settings edits stay in this session only and still apply on the next restart of this process".to_owned()
            }
        };

        RuntimeSettingsSnapshot {
            editable: self.editable.clone(),
            http_bind: self.http_bind.clone(),
            websocket_bind: self.websocket_bind.clone(),
            config_file_path: self.config_file_path.clone(),
            persistence_mode,
            restart_required: true,
            detail,
        }
    }

    fn apply_update(&mut self, settings: RuntimeEditableSettings) {
        self.editable = settings;
    }

    fn file_update(&self) -> RuntimeSettingsFileUpdate {
        RuntimeSettingsFileUpdate {
            startup_mode: self.editable.startup_mode.clone(),
            default_strategy_path: self.editable.default_strategy_path.clone(),
            allow_sqlite_fallback: self.editable.allow_sqlite_fallback,
            paper_account_name: self.editable.paper_account_name.clone(),
            live_account_name: self.editable.live_account_name.clone(),
        }
    }
}

trait RuntimeDispatcherHandle: RuntimeCommandDispatcher {
    fn dispatch_snapshot(&self) -> RuntimeBrokerSnapshot;
    fn append_journal_record(&self, record: EventJournalRecord) -> Result<(), JournalError>;
    fn list_journal_records(&self) -> Result<Vec<EventJournalRecord>, JournalError>;
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

    fn list_journal_records(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
        self.inner.list_journal_records()
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

    fn list_journal_records(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
        self.journal().list()
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
                    chart_bars: BTreeMap::new(),
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
            let chart_bars = seed
                .as_ref()
                .map(|seed| sample_chart_bars_for_strategy(seed, now))
                .unwrap_or_default();
            self.state = RuntimeMarketDataState::Unconfigured {
                detail: if chart_bars.is_empty() {
                    "missing market-data configuration: market_data.api_key".to_owned()
                } else {
                    "missing market-data configuration: market_data.api_key; showing illustrative sample candles until live market data is configured".to_owned()
                },
                chart_bars,
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
                let warmup_mode = strategy_warmup_mode(&seed.strategy, now);
                self.state = RuntimeMarketDataState::Active {
                    service,
                    last_snapshot: Some(snapshot),
                    warmup_mode,
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
                warmup_mode,
            } => service
                .start_warmup(warmup_mode.clone(), now)
                .await
                .map(|snapshot| {
                    *last_snapshot = Some(snapshot.clone());
                    Some(snapshot)
                })
                .map_err(|error| format!("market-data warmup start failed: {error}")),
            RuntimeMarketDataState::Unconfigured { detail, .. }
            | RuntimeMarketDataState::PendingStrategy { detail }
            | RuntimeMarketDataState::StrategyBlocked { detail } => Err(detail.clone()),
            #[cfg(test)]
            RuntimeMarketDataState::SnapshotOverride { snapshot, .. } => {
                let warmup_mode = snapshot.warmup_mode.clone();
                snapshot.warmup_requested = true;
                snapshot.warmup_mode = warmup_mode;
                snapshot.replay_caught_up = snapshot.warmup_mode.replay_from().is_none();
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
                ) && snapshot.replay_caught_up;
                snapshot.updated_at = now;
                Ok(Some(snapshot.clone()))
            }
        }
    }

    fn current_view(&self) -> RuntimeMarketDataView {
        match &self.state {
            RuntimeMarketDataState::Active { last_snapshot, .. } => RuntimeMarketDataView {
                snapshot: last_snapshot.clone(),
                detail: None,
                sample_chart_active: false,
            },
            RuntimeMarketDataState::Unconfigured { detail, chart_bars } => RuntimeMarketDataView {
                snapshot: None,
                detail: Some(detail.clone()),
                sample_chart_active: !chart_bars.is_empty(),
            },
            RuntimeMarketDataState::PendingStrategy { detail }
            | RuntimeMarketDataState::StrategyBlocked { detail } => RuntimeMarketDataView {
                snapshot: None,
                detail: Some(detail.clone()),
                sample_chart_active: false,
            },
            #[cfg(test)]
            RuntimeMarketDataState::SnapshotOverride {
                snapshot, detail, ..
            } => RuntimeMarketDataView {
                snapshot: Some(snapshot.clone()),
                detail: detail.clone(),
                sample_chart_active: false,
            },
        }
    }

    fn chart_bars(
        &self,
        timeframe: Timeframe,
        before: Option<DateTime<Utc>>,
        limit: usize,
    ) -> (Vec<RuntimeChartBar>, bool) {
        let bars = match &self.state {
            RuntimeMarketDataState::Active { service, .. } => service
                .session()
                .coordinator()
                .buffer(timeframe)
                .map(|buffer| {
                    buffer
                        .iter()
                        .filter_map(runtime_chart_bar_from_market_event)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            RuntimeMarketDataState::Unconfigured { chart_bars, .. } => {
                chart_bars.get(&timeframe).cloned().unwrap_or_default()
            }
            RuntimeMarketDataState::PendingStrategy { .. }
            | RuntimeMarketDataState::StrategyBlocked { .. } => Vec::new(),
            #[cfg(test)]
            RuntimeMarketDataState::SnapshotOverride { chart_bars, .. } => {
                chart_bars.get(&timeframe).cloned().unwrap_or_default()
            }
        };

        paginate_chart_bars(bars, before, limit)
    }

    async fn refresh(&mut self, now: chrono::DateTime<Utc>) -> RuntimeMarketDataView {
        match &mut self.state {
            RuntimeMarketDataState::Active {
                service,
                last_snapshot,
                ..
            } => {
                let is_connected = !matches!(
                    service.snapshot(now).session.market_data.connection_state,
                    MarketDataConnectionState::Disconnected
                );
                let mut poll_error_detail = None;
                if is_connected {
                    for _ in 0..MARKET_DATA_POLL_BUDGET {
                        match timeout(MARKET_DATA_POLL_TIMEOUT, service.poll_next_update()).await {
                            Ok(Ok(Some(_))) => continue,
                            Ok(Ok(None)) | Err(_) => break,
                            Ok(Err(error)) => {
                                let detail = format!("market-data service polling failed: {error}");
                                service
                                    .session_mut()
                                    .coordinator_mut()
                                    .set_connection_state(MarketDataConnectionState::Failed, now);
                                service
                                    .session_mut()
                                    .coordinator_mut()
                                    .mark_degraded(detail.clone(), now);
                                warn!(?error, "market-data service polling failed");
                                poll_error_detail = Some(detail);
                                break;
                            }
                        }
                    }
                }

                let snapshot = service.snapshot(now);
                let detail = if matches!(
                    snapshot.session.market_data.connection_state,
                    MarketDataConnectionState::Failed
                ) {
                    Some(poll_error_detail.unwrap_or_else(|| {
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
                            .unwrap_or_else(|| "market-data service reported a failure".to_owned())
                    }))
                } else {
                    None
                };
                *last_snapshot = Some(snapshot.clone());

                RuntimeMarketDataView {
                    snapshot: Some(snapshot),
                    detail,
                    sample_chart_active: false,
                }
            }
            RuntimeMarketDataState::Unconfigured { detail, chart_bars } => RuntimeMarketDataView {
                snapshot: None,
                detail: Some(detail.clone()),
                sample_chart_active: !chart_bars.is_empty(),
            },
            RuntimeMarketDataState::PendingStrategy { detail }
            | RuntimeMarketDataState::StrategyBlocked { detail } => RuntimeMarketDataView {
                snapshot: None,
                detail: Some(detail.clone()),
                sample_chart_active: false,
            },
            #[cfg(test)]
            RuntimeMarketDataState::SnapshotOverride {
                snapshot, detail, ..
            } => RuntimeMarketDataView {
                snapshot: Some(snapshot.clone()),
                detail: detail.clone(),
                sample_chart_active: false,
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
    runtime_settings: Arc<Mutex<RuntimeSettingsState>>,
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

    fn list_journal_records(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
        Ok(Vec::new())
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

#[derive(Clone, Debug, Default, Deserialize)]
struct RuntimeChartQuery {
    timeframe: Option<Timeframe>,
    limit: Option<usize>,
    before: Option<DateTime<Utc>>,
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
    config_path: Option<PathBuf>,
    config: AppConfig,
    runtime: RuntimeStateMachine,
) -> Result<(), RuntimeHostError> {
    let state = build_runtime_host_state_with_config_path(config_path, &config, runtime)?;
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
        .route("/", get(root_handler))
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/readiness", get(readiness_handler))
        .route("/chart/config", get(chart_config_handler))
        .route("/chart/snapshot", get(chart_snapshot_handler))
        .route("/chart/history", get(chart_history_handler))
        .route("/history", get(history_handler))
        .route("/journal", get(journal_handler))
        .route(
            "/settings",
            get(settings_handler).post(update_settings_handler),
        )
        .route("/strategies", get(strategy_library_handler))
        .route("/strategies/upload", post(strategy_upload_handler))
        .route("/strategies/validate", post(strategy_validation_handler))
        .route("/runtime/commands", post(runtime_command_handler))
        .route("/commands", post(command_handler))
        .with_state(state)
}

pub fn build_websocket_router(state: RuntimeHostState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/events", get(websocket_handler))
        .route("/chart/stream", get(chart_websocket_handler))
        .with_state(state)
}

pub fn build_runtime_host_state(
    config: &AppConfig,
    runtime: RuntimeStateMachine,
) -> Result<RuntimeHostState, RuntimeHostError> {
    build_runtime_host_state_with_config_path(None, config, runtime)
}

fn build_runtime_host_state_with_config_path(
    config_file_path: Option<PathBuf>,
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
        runtime_settings: Arc::new(Mutex::new(RuntimeSettingsState::from_config(
            config,
            config_file_path,
        ))),
        shutdown_signal,
        shutdown_review: Arc::new(Mutex::new(ShutdownReviewState::default())),
    })
}

fn normalize_chart_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(CHART_DEFAULT_LIMIT)
        .clamp(1, CHART_MAX_LIMIT)
}

fn runtime_chart_bar_from_market_event(event: &MarketEvent) -> Option<RuntimeChartBar> {
    match event {
        MarketEvent::Bar {
            timeframe,
            open,
            high,
            low,
            close,
            volume,
            closed_at,
            ..
        } => Some(RuntimeChartBar {
            timeframe: *timeframe,
            open: *open,
            high: *high,
            low: *low,
            close: *close,
            volume: *volume,
            closed_at: *closed_at,
        }),
        _ => None,
    }
}

fn paginate_chart_bars(
    bars: Vec<RuntimeChartBar>,
    before: Option<DateTime<Utc>>,
    limit: usize,
) -> (Vec<RuntimeChartBar>, bool) {
    let filtered = if let Some(before) = before {
        bars.into_iter()
            .filter(|bar| bar.closed_at < before)
            .collect::<Vec<_>>()
    } else {
        bars
    };

    let total_available = filtered.len();
    if total_available <= limit {
        return (filtered, false);
    }

    let start = total_available.saturating_sub(limit);
    (filtered[start..].to_vec(), true)
}

fn parse_preferred_chart_timeframe(value: &str) -> Option<Timeframe> {
    match value.trim() {
        "1s" => Some(Timeframe::OneSecond),
        "1m" => Some(Timeframe::OneMinute),
        "5m" => Some(Timeframe::FiveMinute),
        _ => None,
    }
}

fn supported_chart_timeframes(strategy: &tv_bot_core_types::CompiledStrategy) -> Vec<Timeframe> {
    let mut supported = strategy
        .data_requirements
        .timeframes
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    supported.extend(strategy.warmup.bars_required.keys().copied());

    if supported.is_empty() {
        supported.insert(Timeframe::OneMinute);
    }

    supported.into_iter().collect()
}

fn default_chart_timeframe(
    strategy: &tv_bot_core_types::CompiledStrategy,
    supported_timeframes: &[Timeframe],
) -> Option<Timeframe> {
    strategy
        .dashboard_display
        .preferred_chart_timeframe
        .as_deref()
        .and_then(parse_preferred_chart_timeframe)
        .filter(|timeframe| supported_timeframes.contains(timeframe))
        .or_else(|| supported_timeframes.first().copied())
}

fn chart_instrument_summary(
    seed: &crate::operator::LoadedStrategyMarketDataSeed,
) -> RuntimeChartInstrumentSummary {
    let mapping = seed.instrument_mapping.as_ref();

    RuntimeChartInstrumentSummary {
        strategy_id: seed.strategy.metadata.strategy_id.clone(),
        strategy_name: seed.strategy.metadata.name.clone(),
        market_family: seed.strategy.market.market.clone(),
        market_display_name: mapping.map(|value| value.market_display_name.clone()),
        tradovate_symbol: mapping.map(|value| value.tradovate_symbol.clone()),
        canonical_symbol: mapping.map(|value| value.resolved_contract.canonical_symbol.clone()),
        databento_symbols: mapping
            .map(|value| {
                value
                    .databento_symbols
                    .iter()
                    .map(|instrument| instrument.symbol.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        summary: mapping
            .map(|value| value.summary.clone())
            .or_else(|| seed.instrument_resolution_error.clone())
            .unwrap_or_else(|| {
                format!(
                    "loaded strategy `{}` for market `{}`",
                    seed.strategy.metadata.strategy_id, seed.strategy.market.market
                )
            }),
    }
}

fn discover_strategy_library_roots(config: &AppConfig) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(upload_root) = discover_strategy_upload_root() {
        push_strategy_library_root_candidate(&upload_root, &mut roots, &mut seen);
    }

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

fn discover_strategy_upload_root() -> Option<PathBuf> {
    let current_dir = std::env::current_dir().ok()?;
    let mut cursor = current_dir.clone();

    loop {
        let strategies_root = cursor.join("strategies");
        if strategies_root.is_dir() {
            return Some(strategies_root.join("uploads"));
        }

        if !cursor.pop() {
            break;
        }
    }

    Some(current_dir.join("strategies").join("uploads"))
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

fn push_strategy_library_root_candidate(
    candidate: &Path,
    roots: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) {
    let normalized = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    if seen.insert(normalized.clone()) {
        roots.push(normalized);
    }
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

async fn root_handler() -> Json<serde_json::Value> {
    Json(json!({
        "service": "tv-bot-runtime-host",
        "detail": "local control-plane API; use one of the listed routes instead of the bare root URL",
        "routes": [
            "/health",
            "/status",
            "/readiness",
            "/chart/config",
            "/chart/snapshot",
            "/chart/history",
            "/history",
            "/journal",
            "/settings",
            "/strategies",
            "/strategies/upload",
            "/strategies/validate",
            "/runtime/commands",
            "/commands",
            "/events",
            "/chart/stream"
        ]
    }))
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

async fn chart_config_handler(
    State(state): State<RuntimeHostState>,
) -> Json<RuntimeChartConfigResponse> {
    Json(build_chart_config(&state).await)
}

async fn chart_snapshot_handler(
    State(state): State<RuntimeHostState>,
    Query(query): Query<RuntimeChartQuery>,
) -> Response {
    match build_chart_snapshot(&state, query).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(message) => json_message_response(StatusCode::BAD_REQUEST, message),
    }
}

async fn chart_history_handler(
    State(state): State<RuntimeHostState>,
    Query(query): Query<RuntimeChartQuery>,
) -> Response {
    match build_chart_history_response(&state, query).await {
        Ok(history) => Json(history).into_response(),
        Err(message) => json_message_response(StatusCode::BAD_REQUEST, message),
    }
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

async fn journal_handler(State(state): State<RuntimeHostState>) -> Response {
    let records_result = {
        let handler = state.http_handler.lock().await;
        handler.dispatcher().list_journal_records()
    };

    match records_result {
        Ok(records) => {
            let total_records = records.len();
            let records = records
                .into_iter()
                .rev()
                .take(JOURNAL_ROUTE_LIMIT)
                .collect::<Vec<_>>();

            Json(RuntimeJournalSnapshot {
                total_records,
                records,
            })
            .into_response()
        }
        Err(error) => {
            let _ = state.health_supervisor.note_error();
            error!(?error, "runtime host journal handler failed");
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

async fn settings_handler(State(state): State<RuntimeHostState>) -> Json<RuntimeSettingsSnapshot> {
    let settings = state.runtime_settings.lock().await.snapshot();
    Json(settings)
}

async fn update_settings_handler(
    State(state): State<RuntimeHostState>,
    Json(request): Json<RuntimeSettingsUpdateRequest>,
) -> Response {
    let normalized = normalize_runtime_settings(request.settings);
    let source = request.source.into();

    let (config_file_path, next_settings, settings_snapshot, file_update) = {
        let current = state.runtime_settings.lock().await.clone();
        let mut next = current.clone();
        next.apply_update(normalized);
        let snapshot = next.snapshot();
        let file_update = next.file_update();
        (
            current.config_file_path.clone(),
            next,
            snapshot,
            file_update,
        )
    };

    if let Some(path) = config_file_path.as_ref() {
        if let Err(error) = persist_runtime_settings_update(path, &file_update) {
            let _ = state.health_supervisor.note_error();
            warn!(?error, path = %path.display(), "failed to persist runtime settings update");
            journal_host_event(
                &state,
                "config",
                "settings_update_failed",
                source,
                EventSeverity::Error,
                json!({
                    "config_file_path": path,
                    "message": error.to_string(),
                }),
            )
            .await;

            return config_update_error_response(error);
        }
    }

    {
        let mut settings = state.runtime_settings.lock().await;
        *settings = next_settings;
    }

    journal_host_event(
        &state,
        "config",
        "settings_updated",
        source,
        EventSeverity::Info,
        json!({
            "startup_mode": settings_snapshot.editable.startup_mode,
            "default_strategy_path": settings_snapshot.editable.default_strategy_path,
            "allow_sqlite_fallback": settings_snapshot.editable.allow_sqlite_fallback,
            "paper_account_name": settings_snapshot.editable.paper_account_name,
            "live_account_name": settings_snapshot.editable.live_account_name,
            "persistence_mode": settings_snapshot.persistence_mode,
            "config_file_path": settings_snapshot.config_file_path,
            "restart_required": settings_snapshot.restart_required,
        }),
    )
    .await;

    Json(RuntimeSettingsUpdateResponse {
        message: match settings_snapshot.persistence_mode {
            RuntimeSettingsPersistenceMode::ConfigFile => {
                "saved runtime settings for the next restart".to_owned()
            }
            RuntimeSettingsPersistenceMode::SessionOnly => {
                "updated runtime settings for this session; no config file path is available for persistence".to_owned()
            }
        },
        settings: settings_snapshot,
    })
    .into_response()
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

async fn strategy_upload_handler(
    State(state): State<RuntimeHostState>,
    Json(request): Json<RuntimeStrategyUploadRequest>,
) -> Response {
    let upload_root = match state.strategy_library_roots.first().cloned() {
        Some(root) => root,
        None => {
            return json_message_response(
                StatusCode::CONFLICT,
                "no writable strategy library root is available for uploads".to_owned(),
            );
        }
    };
    let source = request.source.into();
    let requested_filename = request.filename.clone();

    match tokio::task::spawn_blocking(move || {
        store_uploaded_strategy(upload_root, request.filename, request.markdown)
    })
    .await
    {
        Ok(Ok(response)) => {
            let severity = if response.valid {
                EventSeverity::Info
            } else {
                EventSeverity::Warning
            };

            journal_host_event(
                &state,
                "strategy",
                "upload_saved",
                source,
                severity,
                json!({
                    "path": response.display_path,
                    "valid": response.valid,
                    "warning_count": response.warnings.len(),
                    "error_count": response.errors.len(),
                    "filename": requested_filename,
                }),
            )
            .await;

            Json(response).into_response()
        }
        Ok(Err(error)) => json_message_response(error.status_code(), error.message()),
        Err(error) => runtime_host_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("strategy upload task failed: {error}"),
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
        RuntimeLifecycleCommand::ClosePosition {
            contract_id,
            reason,
        } => {
            close_position_runtime_command_handler(state, request.source, contract_id, reason).await
        }
        RuntimeLifecycleCommand::ManualEntry {
            side,
            quantity,
            tick_size,
            entry_reference_price,
            tick_value_usd,
            reason,
        } => {
            manual_entry_runtime_command_handler(
                state,
                request.source,
                side,
                quantity,
                tick_size,
                entry_reference_price,
                tick_value_usd,
                reason,
            )
            .await
        }
        RuntimeLifecycleCommand::CancelWorkingOrders { reason } => {
            cancel_working_orders_runtime_command_handler(state, request.source, reason).await
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

    journal_lifecycle_state_command(&state, &history_command, source).await;

    if let Err(error) = sync_history_for_lifecycle_command(&state, &history_command, source).await {
        let _ = state.health_supervisor.note_error();
        warn!(?error, "failed to persist lifecycle history");
    }

    runtime_lifecycle_success_response(&state, HttpStatusCode::Ok, message, None).await
}

async fn journal_lifecycle_state_command(
    state: &RuntimeHostState,
    command: &RuntimeLifecycleCommand,
    source: tv_bot_control_api::ManualCommandSource,
) {
    match command {
        RuntimeLifecycleCommand::SetNewEntriesEnabled { enabled, reason } => {
            journal_host_event(
                state,
                "operator",
                "new_entries_gate_updated",
                source.into(),
                if *enabled {
                    EventSeverity::Info
                } else {
                    EventSeverity::Warning
                },
                json!({
                    "enabled": enabled,
                    "reason": reason,
                }),
            )
            .await;
        }
        _ => {}
    }
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

async fn close_position_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
    contract_id: Option<i64>,
    reason: Option<String>,
) -> Response {
    let context = status_context(&state, true).await;
    let request_result = {
        let operator = state.operator_state.lock().await;
        operator.build_close_position_request(&context, source, contract_id, reason)
    };
    let request = match request_result {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    dispatch_lifecycle_execution_request(
        &state,
        request,
        "close position command dispatched".to_owned(),
    )
    .await
}

async fn manual_entry_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
    side: tv_bot_core_types::TradeSide,
    quantity: u32,
    tick_size: rust_decimal::Decimal,
    entry_reference_price: rust_decimal::Decimal,
    tick_value_usd: Option<rust_decimal::Decimal>,
    reason: Option<String>,
) -> Response {
    let context = status_context(&state, true).await;
    let request_result = {
        let operator = state.operator_state.lock().await;
        operator.build_manual_entry_request(
            &context,
            source,
            side,
            quantity,
            tick_size,
            entry_reference_price,
            tick_value_usd,
            reason,
        )
    };
    let request = match request_result {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    dispatch_lifecycle_execution_request(
        &state,
        request,
        "manual entry command dispatched".to_owned(),
    )
    .await
}

async fn cancel_working_orders_runtime_command_handler(
    state: RuntimeHostState,
    source: tv_bot_control_api::ManualCommandSource,
    reason: Option<String>,
) -> Response {
    let context = status_context(&state, true).await;
    let request_result = {
        let operator = state.operator_state.lock().await;
        operator.build_cancel_working_orders_request(&context, source, reason)
    };
    let request = match request_result {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(&state, error).await,
    };

    dispatch_lifecycle_execution_request(
        &state,
        request,
        "working-order cancellation dispatched".to_owned(),
    )
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
    let context = status_context(state, true).await;
    let request = {
        let operator = state.operator_state.lock().await;
        operator.sanitize_command_request(&context, request)
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => return runtime_lifecycle_error_response(state, error).await,
    };

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

async fn chart_websocket_handler(
    State(state): State<RuntimeHostState>,
    Query(query): Query<RuntimeChartQuery>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| chart_websocket_loop(socket, state, query))
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

async fn chart_websocket_loop(
    mut socket: WebSocket,
    state: RuntimeHostState,
    query: RuntimeChartQuery,
) {
    let mut interval = tokio::time::interval(CHART_STREAM_INTERVAL);
    let mut last_snapshot: Option<RuntimeChartSnapshot> = None;

    loop {
        let snapshot = match build_chart_snapshot(&state, query.clone()).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                error!(?error, "failed to build chart websocket snapshot");
                break;
            }
        };

        if last_snapshot.as_ref() != Some(&snapshot) {
            let payload = match serde_json::to_string(&RuntimeChartStreamEvent::Snapshot {
                snapshot: snapshot.clone(),
                occurred_at: Utc::now(),
            }) {
                Ok(payload) => payload,
                Err(error) => {
                    error!(?error, "failed to serialize chart websocket snapshot");
                    break;
                }
            };

            if socket.send(Message::Text(payload.into())).await.is_err() {
                break;
            }

            last_snapshot = Some(snapshot);
        }

        interval.tick().await;
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

fn normalize_runtime_settings(settings: RuntimeEditableSettings) -> RuntimeEditableSettings {
    RuntimeEditableSettings {
        startup_mode: settings.startup_mode,
        default_strategy_path: normalize_optional_path(settings.default_strategy_path),
        allow_sqlite_fallback: settings.allow_sqlite_fallback,
        paper_account_name: normalize_optional_string(settings.paper_account_name),
        live_account_name: normalize_optional_string(settings.live_account_name),
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

fn normalize_optional_path(value: Option<PathBuf>) -> Option<PathBuf> {
    value.and_then(|value| {
        if value.as_os_str().to_string_lossy().trim().is_empty() {
            None
        } else {
            Some(value)
        }
    })
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

async fn build_chart_config(state: &RuntimeHostState) -> RuntimeChartConfigResponse {
    let seed = {
        let operator = state.operator_state.lock().await;
        operator.market_data_seed().ok()
    };
    let market_data_view = {
        let market_data = state.market_data.lock().await;
        market_data.current_view()
    };

    let market_data_connection_state = market_data_view
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.session.market_data.connection_state);
    let market_data_health = market_data_view
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.session.market_data.health);
    let replay_caught_up = market_data_view
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.replay_caught_up)
        .unwrap_or(false);
    let trade_ready = market_data_view
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.trade_ready)
        .unwrap_or(false);
    let sample_data_active = market_data_view.sample_chart_active;

    match seed {
        None => RuntimeChartConfigResponse {
            available: false,
            detail: "load a strategy to chart the resolved contract".to_owned(),
            sample_data_active,
            instrument: None,
            supported_timeframes: Vec::new(),
            default_timeframe: None,
            market_data_connection_state,
            market_data_health,
            replay_caught_up,
            trade_ready,
        },
        Some(seed) => {
            let supported_timeframes = supported_chart_timeframes(&seed.strategy);
            let default_timeframe = default_chart_timeframe(&seed.strategy, &supported_timeframes);
            let instrument = chart_instrument_summary(&seed);
            let available = seed.instrument_mapping.is_some();
            let detail = if available {
                if sample_data_active {
                    market_data_view.detail.clone().unwrap_or_else(|| {
                        format!(
                            "showing illustrative sample candles for `{}` until live market data is configured",
                            instrument
                                .tradovate_symbol
                                .as_deref()
                                .unwrap_or("unresolved symbol")
                        )
                    })
                } else {
                    format!(
                        "charting the loaded strategy contract `{}`",
                        instrument
                            .tradovate_symbol
                            .as_deref()
                            .unwrap_or("unresolved symbol")
                    )
                }
            } else {
                seed.instrument_resolution_error.unwrap_or_else(|| {
                    "instrument resolution must succeed before the live contract chart is available"
                        .to_owned()
                })
            };

            RuntimeChartConfigResponse {
                available,
                detail,
                sample_data_active,
                instrument: Some(instrument),
                supported_timeframes,
                default_timeframe,
                market_data_connection_state,
                market_data_health,
                replay_caught_up,
                trade_ready,
            }
        }
    }
}

fn resolved_chart_timeframe(
    config: &RuntimeChartConfigResponse,
    requested: Option<Timeframe>,
) -> Result<Timeframe, String> {
    if !config.available {
        return Ok(requested
            .or(config.default_timeframe)
            .unwrap_or(Timeframe::OneMinute));
    }

    let timeframe = requested
        .or(config.default_timeframe)
        .ok_or_else(|| "chart timeframe is unavailable until the strategy exposes at least one supported timeframe".to_owned())?;

    if !config.supported_timeframes.contains(&timeframe) {
        return Err(format!(
            "timeframe `{}` is not supported for the loaded strategy contract",
            serde_json::to_string(&timeframe)
                .unwrap_or_else(|_| "\"unknown\"".to_owned())
                .trim_matches('"')
        ));
    }

    Ok(timeframe)
}

async fn build_chart_snapshot(
    state: &RuntimeHostState,
    query: RuntimeChartQuery,
) -> Result<RuntimeChartSnapshot, String> {
    let config = build_chart_config(state).await;
    let timeframe = resolved_chart_timeframe(&config, query.timeframe)?;
    let requested_limit = normalize_chart_limit(query.limit);
    let dispatch_snapshot = current_dispatch_snapshot(state).await;
    let (bars, can_load_older_history) = {
        let market_data = state.market_data.lock().await;
        market_data.chart_bars(timeframe, query.before, requested_limit)
    };
    let symbol = config
        .instrument
        .as_ref()
        .and_then(|instrument| instrument.tradovate_symbol.as_deref());
    let active_position = symbol.and_then(|symbol| {
        dispatch_snapshot
            .open_positions
            .iter()
            .find(|position| position.symbol == symbol && position.quantity != 0)
            .cloned()
    });
    let working_orders = match symbol {
        Some(symbol) => dispatch_snapshot
            .working_orders
            .iter()
            .filter(|order| order.symbol == symbol)
            .cloned()
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let mut recent_fills = match symbol {
        Some(symbol) => dispatch_snapshot
            .fills
            .iter()
            .filter(|fill| fill.symbol == symbol)
            .cloned()
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    recent_fills.sort_by_key(|fill| fill.occurred_at);
    if recent_fills.len() > CHART_RECENT_FILL_LIMIT {
        recent_fills = recent_fills[recent_fills.len() - CHART_RECENT_FILL_LIMIT..].to_vec();
    }

    Ok(RuntimeChartSnapshot {
        config,
        timeframe,
        requested_limit,
        latest_price: bars.last().map(|bar| bar.close),
        latest_closed_at: bars.last().map(|bar| bar.closed_at),
        bars,
        active_position,
        working_orders,
        recent_fills,
        can_load_older_history,
    })
}

async fn build_chart_history_response(
    state: &RuntimeHostState,
    query: RuntimeChartQuery,
) -> Result<RuntimeChartHistoryResponse, String> {
    let config = build_chart_config(state).await;
    let timeframe = resolved_chart_timeframe(&config, query.timeframe)?;
    let requested_limit = normalize_chart_limit(query.limit);
    let (bars, can_load_older_history) = {
        let market_data = state.market_data.lock().await;
        market_data.chart_bars(timeframe, query.before, requested_limit)
    };

    Ok(RuntimeChartHistoryResponse {
        config,
        timeframe,
        requested_limit,
        before: query.before,
        bars,
        can_load_older_history,
    })
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
    if !root.exists() {
        return Ok(());
    }
    if !root.is_dir() {
        return Err(format!(
            "strategy library root `{}` is not a directory",
            root.display()
        ));
    }

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

#[derive(Debug)]
enum StrategyUploadError {
    InvalidRequest(String),
    Io(String),
}

impl StrategyUploadError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::CONFLICT,
            Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(self) -> String {
        match self {
            Self::InvalidRequest(message) | Self::Io(message) => message,
        }
    }
}

fn store_uploaded_strategy(
    upload_root: PathBuf,
    filename: String,
    markdown: String,
) -> Result<RuntimeStrategyValidationResponse, StrategyUploadError> {
    if markdown.trim().is_empty() {
        return Err(StrategyUploadError::InvalidRequest(
            "uploaded strategy markdown cannot be empty".to_owned(),
        ));
    }

    let sanitized_filename = sanitize_strategy_upload_filename(&filename)?;
    fs::create_dir_all(&upload_root).map_err(|source| {
        StrategyUploadError::Io(format!(
            "failed to prepare strategy upload root `{}`: {source}",
            upload_root.display()
        ))
    })?;

    let path = allocate_strategy_upload_path(&upload_root, &sanitized_filename);
    fs::write(&path, &markdown).map_err(|source| {
        StrategyUploadError::Io(format!(
            "failed to write uploaded strategy file `{}`: {source}",
            path.display()
        ))
    })?;

    Ok(validate_strategy_markdown(path, markdown))
}

fn sanitize_strategy_upload_filename(filename: &str) -> Result<String, StrategyUploadError> {
    let basename = Path::new(filename.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            StrategyUploadError::InvalidRequest(
                "uploaded strategy filename must be a non-empty UTF-8 name".to_owned(),
            )
        })?;

    let mut sanitized = basename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric()
                || character == '.'
                || character == '-'
                || character == '_'
            {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .to_owned();

    if sanitized.is_empty() {
        sanitized = "uploaded_strategy".to_owned();
    }

    if !sanitized.to_ascii_lowercase().ends_with(".md") {
        sanitized.push_str(".md");
    }

    Ok(sanitized)
}

fn allocate_strategy_upload_path(upload_root: &Path, filename: &str) -> PathBuf {
    let filename_path = Path::new(filename);
    let stem = filename_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("uploaded_strategy");
    let extension = filename_path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("md");

    let mut candidate = upload_root.join(filename);
    let mut suffix = 1;
    while candidate.exists() {
        candidate = upload_root.join(format!("{stem}-{suffix}.{extension}"));
        suffix += 1;
    }

    candidate
}

fn validate_strategy_path(path: PathBuf) -> Result<RuntimeStrategyValidationResponse, String> {
    let markdown = fs::read_to_string(&path).map_err(|source| {
        format!(
            "failed to read strategy file `{}`: {source}",
            path.display()
        )
    })?;

    Ok(validate_strategy_markdown(path, markdown))
}

fn validate_strategy_markdown(
    path: PathBuf,
    markdown: String,
) -> RuntimeStrategyValidationResponse {
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

            RuntimeStrategyValidationResponse {
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
            }
        }
        Err(error) => RuntimeStrategyValidationResponse {
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
        },
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

fn config_update_error_response(error: ConfigUpdateError) -> Response {
    runtime_host_error_response(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn json_message_response(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "message": message }))).into_response()
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
        | RuntimeLifecycleCommand::SetNewEntriesEnabled { .. }
        | RuntimeLifecycleCommand::ResolveReconnectReview { .. }
        | RuntimeLifecycleCommand::Shutdown { .. }
        | RuntimeLifecycleCommand::ClosePosition { .. }
        | RuntimeLifecycleCommand::ManualEntry { .. }
        | RuntimeLifecycleCommand::CancelWorkingOrders { .. }
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
    use chrono::TimeZone;
    use http_body_util::BodyExt;
    use rust_decimal::Decimal;
    use secrecy::SecretString;
    use tower::ServiceExt;
    use tv_bot_broker_tradovate::{
        TradovateAccessToken, TradovateAccount, TradovateAccountApi, TradovateAccountListRequest,
        TradovateAuthApi, TradovateAuthRequest, TradovateCancelOrderRequest,
        TradovateCancelOrderResult, TradovateCredentials, TradovateError, TradovateExecutionApi,
        TradovateLiquidatePositionRequest, TradovateLiquidatePositionResult,
        TradovatePlaceOrderRequest, TradovatePlaceOrderResult, TradovatePlaceOsoRequest,
        TradovatePlaceOsoResult, TradovateReconnectDecision, TradovateRoutingPreferences,
        TradovateSessionConfig, TradovateSessionManager, TradovateSyncApi,
        TradovateSyncConnectRequest, TradovateSyncEvent, TradovateSyncSnapshot,
        TradovateUserSyncRequest,
    };
    use tv_bot_config::{AppConfig, MapEnvironment};
    use tv_bot_control_api::{
        ControlApiCommandResult, ControlApiCommandStatus, ManualCommandSource,
        RuntimeJournalSnapshot,
    };
    use tv_bot_core_types::{
        ActionSource, ArmState, BreakEvenRule, BrokerOrderUpdate, BrokerPositionSnapshot,
        BrokerPreference, CompiledStrategy, ContractMode, DailyLossLimit, DashboardDisplay,
        DataFeedRequirement, DataRequirements, EntryOrderType, EntryRules, ExecutionIntent,
        ExecutionSpec, ExitRules, FailsafeRules, FeedType, MarketConfig, MarketSelection,
        PartialTakeProfitRule, PositionSizing, PositionSizingMode, ReadinessCheckStatus,
        ReversalMode, RiskDecision, RiskDecisionStatus, RiskLimits, ScalingConfig, SessionMode,
        SessionRules, SignalCombinationMode, SignalConfirmation, StateBehavior, StrategyMetadata,
        Timeframe, TradeManagement, TrailingRule, WarmupStatus,
    };
    use tv_bot_execution_engine::{
        ExecutionDispatchReport, ExecutionDispatchResult, ExecutionInstrumentContext,
        ExecutionRequest, ExecutionStateContext,
    };
    use tv_bot_health::RuntimeResourceSample;
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
        cancel_orders: Arc<StdMutex<Vec<TradovateCancelOrderRequest>>>,
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

        fn list_journal_records(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
            Ok(Vec::new())
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

        fn list_journal_records(&self) -> Result<Vec<EventJournalRecord>, JournalError> {
            self.inner.journal().list()
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

    fn sync_snapshot_with_position_only() -> TradovateSyncSnapshot {
        let mut snapshot = sample_dispatch_snapshot();
        snapshot.working_orders.clear();

        TradovateSyncSnapshot {
            occurred_at: Utc::now(),
            positions: snapshot.open_positions,
            working_orders: snapshot.working_orders,
            fills: snapshot.fills,
            account_snapshot: snapshot.account_snapshot,
            mismatch_reason: None,
            detail: "synced position only".to_owned(),
        }
    }

    fn sync_snapshot_with_contract_position() -> TradovateSyncSnapshot {
        let mut snapshot = sample_dispatch_snapshot();
        if let Some(position) = snapshot.open_positions.first_mut() {
            position.symbol = "contract:4444".to_owned();
        }

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

    fn sync_snapshot_with_working_orders_only() -> TradovateSyncSnapshot {
        let mut snapshot = sample_dispatch_snapshot();
        snapshot.open_positions.clear();
        snapshot.fills.clear();

        TradovateSyncSnapshot {
            occurred_at: Utc::now(),
            positions: snapshot.open_positions,
            working_orders: snapshot.working_orders,
            fills: snapshot.fills,
            account_snapshot: snapshot.account_snapshot,
            mismatch_reason: None,
            detail: "synced working orders".to_owned(),
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

    async fn sample_session_manager(
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

    async fn sample_session_manager_with_contract_position(
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
                sync_snapshot_with_contract_position(),
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

    async fn sample_session_manager_with_startup_review_required_for_snapshot(
        startup_snapshot: TradovateSyncSnapshot,
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
            snapshots: Arc::new(StdMutex::new(VecDeque::from([startup_snapshot]))),
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
            .expect("startup sync should connect");

        manager
    }

    async fn sample_session_manager_with_startup_review_required_working_orders(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        sample_session_manager_with_startup_review_required_for_snapshot(
            sync_snapshot_with_working_orders_only(),
        )
        .await
    }

    async fn sample_session_manager_with_startup_review_required(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        sample_session_manager_with_startup_review_required_for_snapshot(
            sync_snapshot_with_open_position(),
        )
        .await
    }

    async fn sample_session_manager_with_contract_startup_review_required(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        sample_session_manager_with_startup_review_required_for_snapshot(
            sync_snapshot_with_contract_position(),
        )
        .await
    }

    async fn sample_session_manager_with_reconnect_review_required_for_snapshot(
        reconnect_snapshot: TradovateSyncSnapshot,
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
                reconnect_snapshot,
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

    async fn sample_session_manager_with_reconnect_review_required(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        sample_session_manager_with_reconnect_review_required_for_snapshot(
            sync_snapshot_with_open_position(),
        )
        .await
    }

    async fn sample_session_manager_with_contract_reconnect_review_required(
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        sample_session_manager_with_reconnect_review_required_for_snapshot(
            sync_snapshot_with_contract_position(),
        )
        .await
    }

    #[derive(Clone, Copy, Debug)]
    enum ReviewTriggerPhase {
        Startup,
        Reconnect,
    }

    async fn sample_session_manager_for_review_phase(
        phase: ReviewTriggerPhase,
        snapshot: TradovateSyncSnapshot,
    ) -> TradovateSessionManager<TestAuthApi, TestAccountApi, TestSyncApi> {
        match phase {
            ReviewTriggerPhase::Startup => {
                sample_session_manager_with_startup_review_required_for_snapshot(snapshot).await
            }
            ReviewTriggerPhase::Reconnect => {
                sample_session_manager_with_reconnect_review_required_for_snapshot(snapshot).await
            }
        }
    }

    async fn assert_review_required_snapshot_blocks_arming_through_runtime_host(
        phase: ReviewTriggerPhase,
        scenario_label: &str,
        snapshot: TradovateSyncSnapshot,
        expected_open_position_count: usize,
        expected_working_order_count: usize,
    ) {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_for_review_phase(phase, snapshot).await,
            execution_api.clone(),
            journal,
            history,
            latency_collector,
            health_supervisor,
        );
        let app = build_http_router(state.clone());
        let strategy_path = temp_strategy_path();
        write_strategy_file(&strategy_path);

        let load_request_label = format!("{scenario_label} load strategy request");
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
            &load_request_label,
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
        )
        .await;

        for (label_suffix, command) in [
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
            let request_label = format!("{scenario_label} {label_suffix}");
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
                &request_label,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let status_request_label = format!("{scenario_label} status request");
        let status_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            &status_request_label,
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
            status_before.reconnect_review.reason.as_deref(),
            Some(match phase {
                ReviewTriggerPhase::Startup => {
                    "existing broker-side position or working orders detected at startup"
                }
                ReviewTriggerPhase::Reconnect => {
                    "existing broker-side position or working orders detected after reconnect"
                }
            })
        );
        assert!(status_before.reconnect_review.required);
        assert_eq!(
            status_before.reconnect_review.open_position_count,
            expected_open_position_count
        );
        assert_eq!(
            status_before.reconnect_review.working_order_count,
            expected_working_order_count
        );
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
            Some(match phase {
                ReviewTriggerPhase::Startup => 0,
                ReviewTriggerPhase::Reconnect => 1,
            })
        );

        let arm_request_label = format!("{scenario_label} blocked arm request");
        let blocked_arm_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::Arm {
                            allow_override: true,
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            &arm_request_label,
        )
        .await;

        assert_eq!(blocked_arm_response.status(), StatusCode::CONFLICT);
        let blocked_arm_body = blocked_arm_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let blocked_arm: RuntimeLifecycleResponse =
            serde_json::from_slice(&blocked_arm_body).expect("response json should parse");
        assert_eq!(blocked_arm.status_code, HttpStatusCode::Conflict);
        assert!(blocked_arm
            .message
            .contains("readiness report contains blocking issues"));
        assert!(blocked_arm.command_result.is_none());
        assert!(blocked_arm.status.reconnect_review.required);
        assert_eq!(blocked_arm.status.arm_state, ArmState::Disarmed);
        assert_eq!(
            blocked_arm.status.reconnect_review.open_position_count,
            expected_open_position_count
        );
        assert_eq!(
            blocked_arm.status.reconnect_review.working_order_count,
            expected_working_order_count
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

        let _ = fs::remove_file(strategy_path);
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
        test_state_with_settings(
            dispatcher,
            history,
            latency_collector,
            health_supervisor,
            Vec::new(),
            None,
        )
    }

    fn test_state_with_strategy_roots(
        dispatcher: BoxedDispatcher,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
        strategy_library_roots: Vec<PathBuf>,
    ) -> RuntimeHostState {
        test_state_with_settings(
            dispatcher,
            history,
            latency_collector,
            health_supervisor,
            strategy_library_roots,
            None,
        )
    }

    fn test_state_with_config_path(
        dispatcher: BoxedDispatcher,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
        config_file_path: PathBuf,
    ) -> RuntimeHostState {
        test_state_with_settings(
            dispatcher,
            history,
            latency_collector,
            health_supervisor,
            Vec::new(),
            Some(config_file_path),
        )
    }

    fn test_state_with_settings(
        dispatcher: BoxedDispatcher,
        history: RuntimeHistoryRecorder,
        latency_collector: Arc<RuntimeLatencyCollector>,
        health_supervisor: Arc<RuntimeHealthSupervisor>,
        strategy_library_roots: Vec<PathBuf>,
        config_file_path: Option<PathBuf>,
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
            runtime_settings: Arc::new(Mutex::new(RuntimeSettingsState {
                editable: RuntimeEditableSettings {
                    startup_mode: RuntimeMode::Observation,
                    default_strategy_path: None,
                    allow_sqlite_fallback: false,
                    paper_account_name: Some("paper-primary".to_owned()),
                    live_account_name: Some("live-primary".to_owned()),
                },
                http_bind: "127.0.0.1:8080".to_owned(),
                websocket_bind: "127.0.0.1:8081".to_owned(),
                config_file_path,
            })),
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

    fn sample_chart_bar(
        timeframe: Timeframe,
        closed_at: DateTime<Utc>,
        open: i64,
        high: i64,
        low: i64,
        close: i64,
        volume: u64,
    ) -> RuntimeChartBar {
        RuntimeChartBar {
            timeframe,
            open: Decimal::new(open, 2),
            high: Decimal::new(high, 2),
            low: Decimal::new(low, 2),
            close: Decimal::new(close, 2),
            volume,
            closed_at,
        }
    }

    async fn set_test_market_data_snapshot(
        state: &RuntimeHostState,
        snapshot: MarketDataServiceSnapshot,
        detail: Option<String>,
    ) {
        let mut market_data = state.market_data.lock().await;
        market_data.state = RuntimeMarketDataState::SnapshotOverride {
            snapshot,
            detail,
            chart_bars: BTreeMap::new(),
        };
    }

    async fn set_test_market_data_snapshot_with_chart_bars(
        state: &RuntimeHostState,
        snapshot: MarketDataServiceSnapshot,
        detail: Option<String>,
        chart_bars: BTreeMap<Timeframe, Vec<RuntimeChartBar>>,
    ) {
        let mut market_data = state.market_data.lock().await;
        market_data.state = RuntimeMarketDataState::SnapshotOverride {
            snapshot,
            detail,
            chart_bars,
        };
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

    fn sample_mapping() -> tv_bot_core_types::InstrumentMapping {
        tv_bot_core_types::InstrumentMapping {
            market_family: "gold".to_owned(),
            market_display_name: "COMEX Gold".to_owned(),
            contract_mode: ContractMode::FrontMonthAuto,
            resolved_contract: tv_bot_core_types::FuturesContract {
                market_family: "gold".to_owned(),
                display_name: "COMEX Gold".to_owned(),
                venue: "COMEX".to_owned(),
                symbol_root: "GC".to_owned(),
                month: tv_bot_core_types::ContractMonth {
                    year: 2026,
                    month: 6,
                },
                canonical_symbol: "GCM2026".to_owned(),
            },
            databento_symbols: vec![tv_bot_core_types::DatabentoInstrument {
                dataset: "GLBX.MDP3".to_owned(),
                symbol: "GCM6".to_owned(),
                symbology: tv_bot_core_types::DatabentoSymbology::RawSymbol,
            }],
            tradovate_symbol: "GCM2026".to_owned(),
            resolution_basis: tv_bot_core_types::FrontMonthSelectionBasis::ChainOrder,
            resolved_at: chrono::Utc
                .with_ymd_and_hms(2026, 4, 10, 13, 30, 0)
                .single()
                .expect("timestamp should be valid"),
            summary: "host test mapping".to_owned(),
        }
    }

    #[test]
    fn strategy_warmup_mode_prefers_historical_replay_for_largest_requirement() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 14, 13, 42, 15)
            .single()
            .expect("timestamp should be valid");
        let mut strategy = sample_strategy();
        strategy.warmup.bars_required = std::collections::BTreeMap::from([
            (Timeframe::OneSecond, 600),
            (Timeframe::OneMinute, 100),
            (Timeframe::FiveMinute, 50),
        ]);

        let replay_from = strategy_warmup_replay_from(&strategy, now)
            .expect("historical warmup should compute a replay start");

        assert_eq!(
            replay_from,
            chrono::Utc
                .with_ymd_and_hms(2026, 4, 14, 9, 30, 0)
                .single()
                .expect("timestamp should be valid")
        );
        assert_eq!(
            strategy_warmup_mode(&strategy, now),
            DatabentoWarmupMode::ReplayFrom(replay_from)
        );
    }

    #[test]
    fn market_data_manager_stores_strategy_driven_replay_mode() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 14, 13, 42, 15)
            .single()
            .expect("timestamp should be valid");
        let mut strategy = sample_strategy();
        strategy.warmup.bars_required = std::collections::BTreeMap::from([
            (Timeframe::OneMinute, 10),
            (Timeframe::FiveMinute, 4),
        ]);
        let expected_replay_from = chrono::Utc
            .with_ymd_and_hms(2026, 4, 14, 13, 20, 0)
            .single()
            .expect("timestamp should be valid");

        let config = AppConfig {
            runtime: tv_bot_config::RuntimeConfig {
                startup_mode: RuntimeMode::Observation,
                default_strategy_path: None,
                allow_sqlite_fallback: true,
            },
            market_data: tv_bot_config::MarketDataConfig {
                dataset: Some("GLBX.MDP3".to_owned()),
                gateway: None,
                api_key: Some(SecretString::new("db-test-key".to_owned().into_boxed_str())),
            },
            broker: tv_bot_config::BrokerConfig {
                environment: None,
                http_base_url: None,
                websocket_url: None,
                username: None,
                password: None,
                cid: None,
                sec: None,
                app_id: None,
                app_version: None,
                device_id: None,
                paper_account_name: None,
                live_account_name: None,
            },
            persistence: tv_bot_config::PersistenceConfig {
                primary_url: None,
                sqlite_fallback: tv_bot_config::SqliteFallbackConfig {
                    enabled: true,
                    path: PathBuf::from("data/tv_bot_core.sqlite"),
                },
            },
            control_api: tv_bot_config::ControlApiConfig {
                http_bind: "127.0.0.1:8080".to_owned(),
                websocket_bind: "127.0.0.1:8081".to_owned(),
            },
            logging: tv_bot_config::LoggingConfig {
                level: "info".to_owned(),
                json: false,
            },
        };

        let mut manager = RuntimeMarketDataManager::from_app_config(&config);
        manager.configure_for_strategy(
            Some(LoadedStrategyMarketDataSeed {
                strategy,
                instrument_mapping: Some(sample_mapping()),
                instrument_resolution_error: None,
            }),
            now,
        );

        match manager.state {
            RuntimeMarketDataState::Active { warmup_mode, .. } => {
                assert_eq!(
                    warmup_mode,
                    DatabentoWarmupMode::ReplayFrom(expected_replay_from)
                );
            }
            _ => panic!("market-data manager should be active after strategy configuration"),
        }
    }

    #[tokio::test]
    async fn market_data_refresh_stays_disconnected_before_warmup_starts() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 4, 14, 13, 42, 15)
            .single()
            .expect("timestamp should be valid");
        let config = AppConfig {
            runtime: tv_bot_config::RuntimeConfig {
                startup_mode: RuntimeMode::Observation,
                default_strategy_path: None,
                allow_sqlite_fallback: true,
            },
            market_data: tv_bot_config::MarketDataConfig {
                dataset: Some("GLBX.MDP3".to_owned()),
                gateway: None,
                api_key: Some(SecretString::new("db-test-key".to_owned().into_boxed_str())),
            },
            broker: tv_bot_config::BrokerConfig {
                environment: None,
                http_base_url: None,
                websocket_url: None,
                username: None,
                password: None,
                cid: None,
                sec: None,
                app_id: None,
                app_version: None,
                device_id: None,
                paper_account_name: None,
                live_account_name: None,
            },
            persistence: tv_bot_config::PersistenceConfig {
                primary_url: None,
                sqlite_fallback: tv_bot_config::SqliteFallbackConfig {
                    enabled: true,
                    path: PathBuf::from("data/tv_bot_core.sqlite"),
                },
            },
            control_api: tv_bot_config::ControlApiConfig {
                http_bind: "127.0.0.1:8080".to_owned(),
                websocket_bind: "127.0.0.1:8081".to_owned(),
            },
            logging: tv_bot_config::LoggingConfig {
                level: "info".to_owned(),
                json: false,
            },
        };

        let mut manager = RuntimeMarketDataManager::from_app_config(&config);
        manager.configure_for_strategy(
            Some(LoadedStrategyMarketDataSeed {
                strategy: sample_strategy(),
                instrument_mapping: Some(sample_mapping()),
                instrument_resolution_error: None,
            }),
            now,
        );

        let view = manager.refresh(now).await;
        let snapshot = view
            .snapshot
            .expect("configured market-data snapshot should exist");

        assert!(view.detail.is_none());
        assert_eq!(
            snapshot.session.market_data.connection_state,
            MarketDataConnectionState::Disconnected
        );
        assert_eq!(
            snapshot.session.market_data.health,
            tv_bot_market_data::MarketDataHealth::Disconnected
        );
        assert!(!snapshot.warmup_requested);
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
                broker_order_id: "8102".to_owned(),
                account_id: Some("101".to_owned()),
                symbol: "GCM2026".to_owned(),
                side: Some(tv_bot_core_types::TradeSide::Buy),
                quantity: Some(1),
                order_type: Some(EntryOrderType::Limit),
                status: tv_bot_core_types::BrokerOrderStatus::Working,
                filled_quantity: 0,
                limit_price: Some(Decimal::new(238_650, 2)),
                stop_price: None,
                average_fill_price: None,
                updated_at: chrono::Utc::now(),
            }],
            fills: vec![tv_bot_core_types::BrokerFillUpdate {
                fill_id: "fill-1".to_owned(),
                broker_order_id: Some("8102".to_owned()),
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

    fn temp_config_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("tv_bot_runtime_host_{unique}.toml"))
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
    async fn root_route_lists_control_plane_endpoints() {
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

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
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
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("root payload should parse");

        assert_eq!(payload["service"], "tv-bot-runtime-host");
        assert!(payload["routes"]
            .as_array()
            .expect("routes should be an array")
            .iter()
            .any(|entry| entry == "/settings"));
        assert!(payload["routes"]
            .as_array()
            .expect("routes should be an array")
            .iter()
            .any(|entry| entry == "/events"));
        assert!(payload["routes"]
            .as_array()
            .expect("routes should be an array")
            .iter()
            .any(|entry| entry == "/chart/config"));
        assert!(payload["routes"]
            .as_array()
            .expect("routes should be an array")
            .iter()
            .any(|entry| entry == "/chart/stream"));
    }

    #[tokio::test]
    async fn readiness_route_surfaces_broker_market_data_and_storage_health_for_paper_mode() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let mut state = build_kernel_backed_state_with_manager(
            sample_session_manager().await,
            execution_api,
            journal,
            history,
            latency_collector,
            health_supervisor,
        );
        state.storage_status.fallback_activated = true;
        state.storage_status.allow_runtime_fallback = true;
        state.storage_status.detail =
            "primary Postgres persistence is unavailable; SQLite fallback is active".to_owned();

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
            "readiness route load strategy request",
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
                "readiness route set mode paper request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "readiness route mark warmup ready request",
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
                .uri("/readiness")
                .body(Body::empty())
                .expect("request should build"),
            "readiness route request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let readiness: RuntimeReadinessSnapshot =
            serde_json::from_slice(&body).expect("readiness json should parse");

        assert_eq!(readiness.status.mode, RuntimeMode::Paper);
        assert_eq!(readiness.status.arm_state, ArmState::Disarmed);
        assert_eq!(readiness.status.warmup_status, WarmupStatus::Ready);
        assert_eq!(
            readiness.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(readiness.status.storage_status.fallback_activated);
        assert!(readiness.status.storage_status.allow_runtime_fallback);
        assert_eq!(
            readiness.status.storage_status.detail,
            "primary Postgres persistence is unavailable; SQLite fallback is active"
        );
        assert_eq!(
            readiness
                .status
                .market_data_status
                .as_ref()
                .map(|snapshot| snapshot.session.market_data.health),
            Some(tv_bot_market_data::MarketDataHealth::Healthy)
        );
        assert_eq!(
            readiness
                .status
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.sync_state),
            Some(tv_bot_core_types::BrokerSyncState::Synchronized)
        );

        assert!(readiness.report.hard_override_required);
        assert!(!readiness.report.has_blocking_issues());
        assert!(readiness.report.checks.iter().any(|check| {
            check.name == "account_selected" && check.status == ReadinessCheckStatus::Pass
        }));
        assert!(readiness.report.checks.iter().any(|check| {
            check.name == "market_data" && check.status == ReadinessCheckStatus::Pass
        }));
        assert!(readiness.report.checks.iter().any(|check| {
            check.name == "broker_sync" && check.status == ReadinessCheckStatus::Pass
        }));
        assert!(readiness.report.checks.iter().any(|check| {
            check.name == "storage"
                && check.status == ReadinessCheckStatus::Warning
                && check.message.contains(
                    "primary Postgres persistence is unavailable; SQLite fallback is active",
                )
        }));
        assert!(readiness.report.checks.iter().any(|check| {
            check.name.starts_with("override_requirement_")
                && check.status == ReadinessCheckStatus::Warning
                && check.message.contains(
                    "primary Postgres persistence is unavailable; SQLite fallback is active",
                )
        }));

        let _ = fs::remove_file(strategy_path);
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
    async fn strategy_upload_route_saves_markdown_returns_validation_and_updates_library() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let strategy_root = temp_strategy_library_root();
        let mut state = build_kernel_backed_state(
            execution_api,
            journal.clone(),
            history,
            latency_collector,
            health_supervisor,
        )
        .await;
        state.strategy_library_roots = vec![strategy_root.clone()];
        let app = build_http_router(state);
        let markdown = include_str!("../../../strategies/examples/gc_momentum_fade_v1.md");

        let response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/strategies/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeStrategyUploadRequest {
                        source: ManualCommandSource::Dashboard,
                        filename: "gc uploaded.md".to_owned(),
                        markdown: markdown.to_owned(),
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "strategy upload request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let upload_validation: RuntimeStrategyValidationResponse =
            serde_json::from_slice(&body).expect("upload json should parse");

        assert!(upload_validation.valid);
        assert_eq!(
            upload_validation
                .summary
                .as_ref()
                .map(|summary| summary.strategy_id.as_str()),
            Some("gc_momentum_fade_v1")
        );
        assert_eq!(
            upload_validation
                .path
                .parent()
                .expect("uploaded file should have parent"),
            strategy_root.as_path()
        );
        assert!(upload_validation.path.exists());
        assert_eq!(
            fs::read_to_string(&upload_validation.path).expect("uploaded strategy should read"),
            markdown
        );

        let library_response = request_with_timeout(
            app,
            Request::builder()
                .uri("/strategies")
                .body(Body::empty())
                .expect("request should build"),
            "strategy library request after upload",
        )
        .await;

        assert_eq!(library_response.status(), StatusCode::OK);
        let library_body = library_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let library: RuntimeStrategyLibraryResponse =
            serde_json::from_slice(&library_body).expect("library json should parse");
        assert_eq!(library.scanned_roots, vec![strategy_root.clone()]);
        assert!(library
            .strategies
            .iter()
            .any(|entry| entry.path == upload_validation.path && entry.valid));

        let journal_records = journal.list().expect("journal should list records");
        let upload_record = journal_records
            .iter()
            .find(|record| record.action == "upload_saved")
            .expect("upload should be journaled");
        assert_eq!(upload_record.category, "strategy");
        assert_eq!(upload_record.source, ActionSource::Dashboard);
        assert_eq!(upload_record.payload["valid"].as_bool(), Some(true));
        assert_eq!(
            upload_record.payload["path"].as_str(),
            Some(upload_validation.display_path.as_str())
        );

        let _ = fs::remove_dir_all(strategy_root);
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
    async fn close_position_command_resolves_active_contract_and_dispatches_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_contract_position().await,
            execution_api.clone(),
            journal,
            history,
            latency_collector,
            health_supervisor,
        );
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
                "close position setup request",
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = request_with_timeout(
            app,
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ClosePosition {
                            contract_id: None,
                            reason: Some("dashboard close".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "close position request",
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
            "close position command dispatched"
        );
        assert_eq!(
            lifecycle_response
                .command_result
                .expect("close position should return a command result")
                .status,
            ControlApiCommandStatus::Executed
        );

        let liquidations = execution_api
            .liquidations
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(liquidations.len(), 1);
        assert_eq!(liquidations[0].context.account_id, 101);
        assert_eq!(liquidations[0].contract_id, 4444);
        drop(liquidations);

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn cancel_working_orders_command_dispatches_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_contract_position().await,
            execution_api.clone(),
            journal.clone(),
            history,
            latency_collector,
            health_supervisor,
        );
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
                "cancel working orders setup request",
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = request_with_timeout(
            app,
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::CancelWorkingOrders {
                            reason: Some("dashboard cancel stale orders".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "cancel working orders request",
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
            "working-order cancellation dispatched"
        );
        assert_eq!(
            lifecycle_response
                .command_result
                .expect("cancel working orders should return a command result")
                .status,
            ControlApiCommandStatus::Executed
        );

        let cancel_orders = execution_api
            .cancel_orders
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(cancel_orders.len(), 1);
        assert_eq!(cancel_orders[0].context.account_id, 101);
        assert_eq!(cancel_orders[0].order_id, 8102);
        drop(cancel_orders);

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
        let mut state = build_kernel_backed_state(
            execution_api,
            journal,
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        )
        .await;
        state.storage_status.fallback_activated = true;
        state.storage_status.allow_runtime_fallback = true;
        state.storage_status.detail =
            "primary Postgres persistence is unavailable; SQLite fallback is active".to_owned();
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
    async fn manual_entry_command_dispatches_broker_side_brackets_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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

        let response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_510, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("manual paper entry".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "manual entry runtime command request",
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
            "manual entry command dispatched"
        );
        let command_result = lifecycle_response
            .command_result
            .expect("manual entry should return a command result");
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

        let journal_actions = journal
            .list()
            .expect("journal should list records")
            .into_iter()
            .map(|record| record.action)
            .collect::<Vec<_>>();
        assert!(journal_actions.contains(&"intent_received".to_owned()));
        assert!(journal_actions.contains(&"decision".to_owned()));
        assert!(journal_actions.contains(&"dispatch_succeeded".to_owned()));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_manual_entry_requires_arm_before_dispatch_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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
            "paper manual entry requires arm load strategy request",
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
                "paper manual entry requires arm set mode request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "paper manual entry requires arm mark warmup ready request",
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

        let status_before_arm = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "paper manual entry requires arm status before arm request",
        )
        .await;
        assert_eq!(status_before_arm.status(), StatusCode::OK);
        let status_before_arm_body = status_before_arm
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status_before_arm: RuntimeStatusSnapshot =
            serde_json::from_slice(&status_before_arm_body).expect("status json should parse");
        assert_eq!(status_before_arm.mode, RuntimeMode::Paper);
        assert_eq!(status_before_arm.arm_state, ArmState::Disarmed);
        assert_eq!(
            status_before_arm.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert_eq!(
            status_before_arm
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
            "paper manual entry requires arm history before request",
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

        let blocked_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_510, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("blocked while still disarmed".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper manual entry requires arm blocked entry request",
        )
        .await;

        assert_eq!(
            blocked_entry_response.status(),
            StatusCode::PRECONDITION_REQUIRED
        );
        let blocked_entry_body = blocked_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let blocked_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&blocked_entry_body).expect("response json should parse");
        assert_eq!(
            blocked_entry.status_code,
            HttpStatusCode::PreconditionRequired
        );
        assert!(blocked_entry.command_result.is_none());
        assert_eq!(blocked_entry.status.mode, RuntimeMode::Paper);
        assert_eq!(blocked_entry.status.arm_state, ArmState::Disarmed);

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

        let history_after_block = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper manual entry requires arm history after blocked request",
        )
        .await;
        assert_eq!(history_after_block.status(), StatusCode::OK);
        let history_after_block_body = history_after_block
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_block: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_block_body).expect("history json should parse");
        assert_eq!(
            history_after_block.projection.total_order_records,
            history_before.projection.total_order_records
        );

        let arm_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::Arm {
                            allow_override: true,
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper manual entry requires arm arm request",
        )
        .await;
        assert_eq!(arm_response.status(), StatusCode::OK);
        let arm_body = arm_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let arm_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&arm_body).expect("response json should parse");
        assert_eq!(arm_response.status_code, HttpStatusCode::Ok);
        assert!(arm_response.message.contains("runtime armed"));
        assert_eq!(arm_response.status.arm_state, ArmState::Armed);

        let allowed_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_620, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("allowed after explicit arm".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper manual entry requires arm allowed entry request",
        )
        .await;
        assert_eq!(allowed_entry_response.status(), StatusCode::OK);
        let allowed_entry_body = allowed_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let allowed_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&allowed_entry_body).expect("response json should parse");
        assert_eq!(allowed_entry.status_code, HttpStatusCode::Ok);
        assert_eq!(allowed_entry.message, "manual entry command dispatched");
        let command_result = allowed_entry
            .command_result
            .expect("manual entry should return a command result after arm");
        assert_eq!(command_result.status, ControlApiCommandStatus::Executed);
        assert_eq!(command_result.risk_status, RiskDecisionStatus::Accepted);
        assert!(command_result.dispatch_performed);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(place_osos.len(), 1);
        assert_eq!(place_osos[0].context.account_id, 101);
        assert_eq!(place_osos[0].context.account_spec, "paper-primary");
        assert_eq!(place_osos[0].order.symbol, "GCM2026");
        assert_eq!(place_osos[0].order.quantity, 1);
        assert_eq!(place_osos[0].order.brackets.len(), 2);
        drop(place_osos);

        let history_after_allowed = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper manual entry requires arm history after allowed request",
        )
        .await;
        assert_eq!(history_after_allowed.status(), StatusCode::OK);
        let history_after_allowed_body = history_after_allowed
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_allowed: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_allowed_body).expect("history json should parse");
        assert!(
            history_after_allowed.projection.total_order_records
                > history_before.projection.total_order_records
        );

        let journal_actions = journal
            .list()
            .expect("journal should list records")
            .into_iter()
            .map(|record| record.action)
            .collect::<Vec<_>>();
        assert!(journal_actions.contains(&"dispatch_succeeded".to_owned()));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn operator_new_entry_gate_blocks_and_reenables_paper_manual_entry_through_runtime_host()
    {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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
            "load strategy for new-entry gate request",
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
                "set mode paper for new-entry gate request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "mark warmup ready for new-entry gate request",
                RuntimeLifecycleCommand::MarkWarmupReady,
            ),
            (
                "arm runtime for new-entry gate request",
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

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "new-entry gate history before request",
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

        let disable_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::SetNewEntriesEnabled {
                            enabled: false,
                            reason: Some(
                                "let the current runner finish without adding size".to_owned(),
                            ),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "disable new entries request",
        )
        .await;
        assert_eq!(disable_response.status(), StatusCode::OK);
        let disable_body = disable_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let disable_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&disable_body).expect("response json should parse");
        assert_eq!(disable_response.status_code, HttpStatusCode::Ok);
        assert_eq!(
            disable_response.message,
            "new entries disabled: let the current runner finish without adding size"
        );
        assert!(!disable_response.status.operator_new_entries_enabled);
        assert_eq!(
            disable_response
                .status
                .operator_new_entries_reason
                .as_deref(),
            Some("let the current runner finish without adding size")
        );
        assert!(disable_response
            .readiness
            .report
            .checks
            .iter()
            .any(|check| {
                check.name == "operator_entry_gate"
                    && check.status == tv_bot_core_types::ReadinessCheckStatus::Warning
                    && check
                        .message
                        .contains("new entries are disabled by operator control")
            }));

        let blocked_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_510, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("blocked by dashboard gate".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "blocked manual entry request",
        )
        .await;

        assert_eq!(blocked_entry_response.status(), StatusCode::CONFLICT);
        let blocked_entry_body = blocked_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let blocked_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&blocked_entry_body).expect("response json should parse");
        assert_eq!(blocked_entry.status_code, HttpStatusCode::Conflict);
        assert!(blocked_entry.message.contains("new entries are blocked"));
        assert!(blocked_entry.command_result.is_none());
        assert!(!blocked_entry.status.operator_new_entries_enabled);

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

        let history_after_block = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "new-entry gate history after blocked request",
        )
        .await;
        assert_eq!(history_after_block.status(), StatusCode::OK);
        let history_after_block_body = history_after_block
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_block: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_block_body).expect("history json should parse");
        assert_eq!(
            history_after_block.projection.total_order_records,
            history_before.projection.total_order_records
        );

        let enable_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::SetNewEntriesEnabled {
                            enabled: true,
                            reason: Some("resume standard paper entries".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "enable new entries request",
        )
        .await;
        assert_eq!(enable_response.status(), StatusCode::OK);
        let enable_body = enable_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let enable_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&enable_body).expect("response json should parse");
        assert_eq!(enable_response.status_code, HttpStatusCode::Ok);
        assert_eq!(enable_response.message, "new entries enabled");
        assert!(enable_response.status.operator_new_entries_enabled);
        assert_eq!(enable_response.status.operator_new_entries_reason, None);

        let allowed_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_510, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("manual paper entry after re-enable".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "allowed manual entry after re-enable request",
        )
        .await;

        assert_eq!(allowed_entry_response.status(), StatusCode::OK);
        let allowed_entry_body = allowed_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let allowed_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&allowed_entry_body).expect("response json should parse");
        assert_eq!(allowed_entry.status_code, HttpStatusCode::Ok);
        assert_eq!(allowed_entry.message, "manual entry command dispatched");
        let command_result = allowed_entry
            .command_result
            .expect("manual entry should return a command result after re-enable");
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
        drop(place_osos);

        let history_after = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "new-entry gate history after success request",
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

        let journal_records = journal.list().expect("journal should list records");
        let gate_updates = journal_records
            .iter()
            .filter(|record| record.action == "new_entries_gate_updated")
            .collect::<Vec<_>>();
        assert_eq!(gate_updates.len(), 2);
        assert_eq!(gate_updates[0].payload["enabled"].as_bool(), Some(false));
        assert_eq!(
            gate_updates[0].payload["reason"].as_str(),
            Some("let the current runner finish without adding size")
        );
        assert_eq!(gate_updates[1].payload["enabled"].as_bool(), Some(true));
        let journal_actions = journal_records
            .iter()
            .map(|record| record.action.clone())
            .collect::<Vec<_>>();
        assert!(journal_actions.contains(&"intent_received".to_owned()));
        assert!(journal_actions.contains(&"decision".to_owned()));
        assert!(journal_actions.contains(&"dispatch_failed".to_owned()));
        assert!(journal_actions.contains(&"dispatch_succeeded".to_owned()));
        assert!(journal_records.iter().any(|record| {
            record.action == "dispatch_failed"
                && record.payload["error"]
                    .as_str()
                    .is_some_and(|message| message.contains("new entries are blocked"))
        }));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_operator_session_supports_repeated_entries_and_safety_gates_through_runtime_host(
    ) {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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
            "paper session regression load strategy request",
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
                "paper session regression set mode request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "paper session regression mark warmup ready request",
                RuntimeLifecycleCommand::MarkWarmupReady,
            ),
            (
                "paper session regression arm request",
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

        let status_ready = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "paper session regression ready status request",
        )
        .await;
        assert_eq!(status_ready.status(), StatusCode::OK);
        let status_ready_body = status_ready
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let status_ready: RuntimeStatusSnapshot =
            serde_json::from_slice(&status_ready_body).expect("status json should parse");
        assert_eq!(status_ready.mode, RuntimeMode::Paper);
        assert_eq!(status_ready.arm_state, ArmState::Armed);
        assert_eq!(
            status_ready.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(status_ready.operator_new_entries_enabled);
        assert_eq!(
            status_ready
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
            "paper session regression history before request",
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

        let first_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_510, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("first paper regression entry".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper session regression first entry request",
        )
        .await;
        assert_eq!(first_entry_response.status(), StatusCode::OK);
        let first_entry_body = first_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let first_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&first_entry_body).expect("response json should parse");
        assert_eq!(first_entry.status_code, HttpStatusCode::Ok);
        assert_eq!(first_entry.message, "manual entry command dispatched");
        assert_eq!(first_entry.status.mode, RuntimeMode::Paper);
        assert_eq!(
            first_entry.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        let first_command_result = first_entry
            .command_result
            .expect("first manual entry should return a command result");
        assert_eq!(
            first_command_result.status,
            ControlApiCommandStatus::Executed
        );
        assert_eq!(
            first_command_result.risk_status,
            RiskDecisionStatus::Accepted
        );
        assert!(first_command_result.dispatch_performed);

        let history_after_first = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper session regression history after first entry request",
        )
        .await;
        assert_eq!(history_after_first.status(), StatusCode::OK);
        let history_after_first_body = history_after_first
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_first: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_first_body).expect("history json should parse");
        assert!(
            history_after_first.projection.total_order_records
                > history_before.projection.total_order_records
        );
        assert!(history_after_first.projection.latest_order.is_some());

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(place_osos.len(), 1);
        let first_oso = &place_osos[0];
        assert_eq!(first_oso.context.account_id, 101);
        assert_eq!(first_oso.context.account_spec, "paper-primary");
        assert_eq!(first_oso.order.symbol, "GCM2026");
        assert_eq!(first_oso.order.quantity, 1);
        assert_eq!(
            first_oso.order.order_type,
            tv_bot_broker_tradovate::TradovateOrderType::Market
        );
        assert_eq!(first_oso.order.brackets.len(), 2);
        drop(place_osos);

        let disable_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::SetNewEntriesEnabled {
                            enabled: false,
                            reason: Some(
                                "pause fresh paper entries while validating the session".to_owned(),
                            ),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper session regression disable new entries request",
        )
        .await;
        assert_eq!(disable_response.status(), StatusCode::OK);
        let disable_body = disable_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let disable_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&disable_body).expect("response json should parse");
        assert_eq!(disable_response.status_code, HttpStatusCode::Ok);
        assert!(!disable_response.status.operator_new_entries_enabled);

        let blocked_by_gate_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_620, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("blocked by paper operator gate".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper session regression blocked-by-gate entry request",
        )
        .await;
        assert_eq!(blocked_by_gate_response.status(), StatusCode::CONFLICT);
        let blocked_by_gate_body = blocked_by_gate_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let blocked_by_gate: RuntimeLifecycleResponse =
            serde_json::from_slice(&blocked_by_gate_body).expect("response json should parse");
        assert_eq!(blocked_by_gate.status_code, HttpStatusCode::Conflict);
        assert!(blocked_by_gate.message.contains("new entries are blocked"));
        assert!(blocked_by_gate.command_result.is_none());
        assert!(!blocked_by_gate.status.operator_new_entries_enabled);

        let history_after_gate_block = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper session regression history after gate block request",
        )
        .await;
        assert_eq!(history_after_gate_block.status(), StatusCode::OK);
        let history_after_gate_block_body = history_after_gate_block
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_gate_block: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_gate_block_body)
                .expect("history json should parse");
        assert_eq!(
            history_after_gate_block.projection.total_order_records,
            history_after_first.projection.total_order_records
        );

        let enable_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::SetNewEntriesEnabled {
                            enabled: true,
                            reason: Some("resume regression session entries".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper session regression enable new entries request",
        )
        .await;
        assert_eq!(enable_response.status(), StatusCode::OK);
        let enable_body = enable_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let enable_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&enable_body).expect("response json should parse");
        assert_eq!(enable_response.status_code, HttpStatusCode::Ok);
        assert!(enable_response.status.operator_new_entries_enabled);

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
            "paper session regression degraded status request",
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
        assert!(status_degraded.operator_new_entries_enabled);
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

        let blocked_by_health_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_730, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("blocked by degraded market data".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper session regression blocked-by-health entry request",
        )
        .await;
        assert_eq!(blocked_by_health_response.status(), StatusCode::CONFLICT);
        let blocked_by_health_body = blocked_by_health_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let blocked_by_health: RuntimeLifecycleResponse =
            serde_json::from_slice(&blocked_by_health_body).expect("response json should parse");
        assert_eq!(blocked_by_health.status_code, HttpStatusCode::Conflict);
        assert!(blocked_by_health
            .message
            .contains("new entries are blocked"));
        assert!(blocked_by_health.command_result.is_none());
        assert!(blocked_by_health.status.operator_new_entries_enabled);
        assert_eq!(
            blocked_by_health.status.market_data_detail.as_deref(),
            Some("market data is degraded; new entries must remain blocked")
        );

        let history_after_health_block = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper session regression history after health block request",
        )
        .await;
        assert_eq!(history_after_health_block.status(), StatusCode::OK);
        let history_after_health_block_body = history_after_health_block
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_health_block: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_health_block_body)
                .expect("history json should parse");
        assert_eq!(
            history_after_health_block.projection.total_order_records,
            history_after_first.projection.total_order_records
        );

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
        )
        .await;

        let second_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_840, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("paper regression entry after health restore".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper session regression second entry request",
        )
        .await;
        assert_eq!(second_entry_response.status(), StatusCode::OK);
        let second_entry_body = second_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let second_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&second_entry_body).expect("response json should parse");
        assert_eq!(second_entry.status_code, HttpStatusCode::Ok);
        assert_eq!(second_entry.message, "manual entry command dispatched");
        let second_command_result = second_entry
            .command_result
            .expect("second manual entry should return a command result");
        assert_eq!(
            second_command_result.status,
            ControlApiCommandStatus::Executed
        );
        assert_eq!(
            second_command_result.risk_status,
            RiskDecisionStatus::Accepted
        );
        assert!(second_command_result.dispatch_performed);

        let place_osos = execution_api
            .place_osos
            .lock()
            .expect("execution mutex should not poison");
        assert_eq!(place_osos.len(), 2);
        let second_oso = &place_osos[1];
        assert_eq!(second_oso.context.account_id, 101);
        assert_eq!(second_oso.context.account_spec, "paper-primary");
        assert_eq!(second_oso.order.symbol, "GCM2026");
        assert_eq!(second_oso.order.quantity, 1);
        assert_eq!(second_oso.order.brackets.len(), 2);
        drop(place_osos);

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

        let history_after_second = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper session regression history after second entry request",
        )
        .await;
        assert_eq!(history_after_second.status(), StatusCode::OK);
        let history_after_second_body = history_after_second
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_after_second: RuntimeHistorySnapshot =
            serde_json::from_slice(&history_after_second_body).expect("history json should parse");
        assert!(
            history_after_second.projection.total_order_records
                > history_after_first.projection.total_order_records
        );
        assert!(history_after_second.projection.latest_order.is_some());

        let final_status = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "paper session regression final status request",
        )
        .await;
        assert_eq!(final_status.status(), StatusCode::OK);
        let final_status_body = final_status
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let final_status: RuntimeStatusSnapshot =
            serde_json::from_slice(&final_status_body).expect("status json should parse");
        assert_eq!(final_status.mode, RuntimeMode::Paper);
        assert_eq!(final_status.arm_state, ArmState::Armed);
        assert!(final_status.operator_new_entries_enabled);
        assert_eq!(
            final_status
                .market_data_status
                .as_ref()
                .map(|snapshot| snapshot.session.market_data.health),
            Some(tv_bot_market_data::MarketDataHealth::Healthy)
        );

        let journal_records = journal.list().expect("journal should list records");
        let dispatch_succeeded = journal_records
            .iter()
            .filter(|record| record.action == "dispatch_succeeded")
            .count();
        let dispatch_failed = journal_records
            .iter()
            .filter(|record| record.action == "dispatch_failed")
            .count();
        let gate_updates = journal_records
            .iter()
            .filter(|record| record.action == "new_entries_gate_updated")
            .collect::<Vec<_>>();
        assert_eq!(dispatch_succeeded, 2);
        assert_eq!(dispatch_failed, 2);
        assert_eq!(gate_updates.len(), 2);
        assert_eq!(gate_updates[0].payload["enabled"].as_bool(), Some(false));
        assert_eq!(
            gate_updates[0].payload["reason"].as_str(),
            Some("pause fresh paper entries while validating the session")
        );
        assert_eq!(gate_updates[1].payload["enabled"].as_bool(), Some(true));
        assert!(journal_records.iter().any(|record| {
            record.action == "dispatch_failed"
                && record.payload["error"]
                    .as_str()
                    .is_some_and(|message| message.contains("new entries are blocked"))
        }));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_release_sweep_covers_session_gating_and_review_resolution_through_runtime_host()
    {
        {
            let history = test_history();
            let latency_collector = test_latency_collector();
            let health_supervisor = test_health_supervisor();
            let execution_api = TestExecutionApi::default();
            let journal = InMemoryJournal::new();
            let state = build_kernel_backed_state_with_manager(
                sample_session_manager().await,
                execution_api.clone(),
                journal.clone(),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
            );
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
                "paper release sweep session load strategy request",
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
                    "paper release sweep session set mode request",
                    RuntimeLifecycleCommand::SetMode {
                        mode: RuntimeMode::Paper,
                    },
                ),
                (
                    "paper release sweep session mark warmup ready request",
                    RuntimeLifecycleCommand::MarkWarmupReady,
                ),
                (
                    "paper release sweep session arm request",
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

            let history_before = request_with_timeout(
                app.clone(),
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("request should build"),
                "paper release sweep session history before request",
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

            for (label, expected_status, expected_message, command) in [
                (
                    "paper release sweep session first entry request",
                    StatusCode::OK,
                    Some("manual entry command dispatched"),
                    RuntimeLifecycleCommand::ManualEntry {
                        side: tv_bot_core_types::TradeSide::Buy,
                        quantity: 1,
                        tick_size: Decimal::new(10, 1),
                        entry_reference_price: Decimal::new(238_510, 2),
                        tick_value_usd: Some(Decimal::new(10, 0)),
                        reason: Some("paper release sweep first entry".to_owned()),
                    },
                ),
                (
                    "paper release sweep session disable gate request",
                    StatusCode::OK,
                    None,
                    RuntimeLifecycleCommand::SetNewEntriesEnabled {
                        enabled: false,
                        reason: Some("hold fresh entries during release sweep".to_owned()),
                    },
                ),
                (
                    "paper release sweep session blocked gate entry request",
                    StatusCode::CONFLICT,
                    Some("new entries are blocked"),
                    RuntimeLifecycleCommand::ManualEntry {
                        side: tv_bot_core_types::TradeSide::Buy,
                        quantity: 1,
                        tick_size: Decimal::new(10, 1),
                        entry_reference_price: Decimal::new(238_620, 2),
                        tick_value_usd: Some(Decimal::new(10, 0)),
                        reason: Some("blocked by release sweep gate".to_owned()),
                    },
                ),
                (
                    "paper release sweep session re-enable gate request",
                    StatusCode::OK,
                    None,
                    RuntimeLifecycleCommand::SetNewEntriesEnabled {
                        enabled: true,
                        reason: Some("resume release sweep entries".to_owned()),
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
                                source: ManualCommandSource::Dashboard,
                                command,
                            })
                            .expect("request should serialize"),
                        ))
                        .expect("request should build"),
                    label,
                )
                .await;
                assert_eq!(response.status(), expected_status);
                let body = response
                    .into_body()
                    .collect()
                    .await
                    .expect("body should collect")
                    .to_bytes();
                let lifecycle_response: RuntimeLifecycleResponse =
                    serde_json::from_slice(&body).expect("response json should parse");
                if let Some(expected_message) = expected_message {
                    assert!(lifecycle_response.message.contains(expected_message));
                }
            }

            set_test_market_data_snapshot(
                &state,
                sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Degraded),
                Some("market data is degraded; new entries must remain blocked".to_owned()),
            )
            .await;

            let degraded_entry_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Dashboard,
                            command: RuntimeLifecycleCommand::ManualEntry {
                                side: tv_bot_core_types::TradeSide::Buy,
                                quantity: 1,
                                tick_size: Decimal::new(10, 1),
                                entry_reference_price: Decimal::new(238_730, 2),
                                tick_value_usd: Some(Decimal::new(10, 0)),
                                reason: Some("blocked by degraded release sweep feed".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep session degraded entry request",
            )
            .await;
            assert_eq!(degraded_entry_response.status(), StatusCode::CONFLICT);

            set_test_market_data_snapshot(
                &state,
                sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
                None,
            )
            .await;

            let recovered_entry_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Dashboard,
                            command: RuntimeLifecycleCommand::ManualEntry {
                                side: tv_bot_core_types::TradeSide::Buy,
                                quantity: 1,
                                tick_size: Decimal::new(10, 1),
                                entry_reference_price: Decimal::new(238_840, 2),
                                tick_value_usd: Some(Decimal::new(10, 0)),
                                reason: Some("paper release sweep recovered entry".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep session recovered entry request",
            )
            .await;
            assert_eq!(recovered_entry_response.status(), StatusCode::OK);

            let place_osos = execution_api
                .place_osos
                .lock()
                .expect("execution mutex should not poison");
            assert_eq!(place_osos.len(), 2);
            drop(place_osos);

            let cancel_orders = execution_api
                .cancel_orders
                .lock()
                .expect("execution mutex should not poison");
            assert!(cancel_orders.is_empty());
            drop(cancel_orders);

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
                "paper release sweep session history after request",
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

            let journal_records = journal.list().expect("journal should list records");
            assert_eq!(
                journal_records
                    .iter()
                    .filter(|record| record.action == "dispatch_succeeded")
                    .count(),
                2
            );
            assert_eq!(
                journal_records
                    .iter()
                    .filter(|record| record.action == "dispatch_failed")
                    .count(),
                2
            );
            assert_eq!(
                journal_records
                    .iter()
                    .filter(|record| record.action == "new_entries_gate_updated")
                    .count(),
                2
            );

            let _ = fs::remove_file(strategy_path);
        }

        {
            let history = test_history();
            let latency_collector = test_latency_collector();
            let health_supervisor = test_health_supervisor();
            let execution_api = TestExecutionApi::default();
            let journal = InMemoryJournal::new();
            let state = build_kernel_backed_state_with_manager(
                sample_session_manager_with_contract_startup_review_required().await,
                execution_api.clone(),
                journal.clone(),
                history.clone(),
                latency_collector.clone(),
                health_supervisor.clone(),
            );
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
                "paper release sweep startup load strategy request",
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
                    "paper release sweep startup set mode request",
                    RuntimeLifecycleCommand::SetMode {
                        mode: RuntimeMode::Paper,
                    },
                ),
                (
                    "paper release sweep startup mark warmup ready request",
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

            let history_before = request_with_timeout(
                app.clone(),
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("request should build"),
                "paper release sweep startup history before request",
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

            let blocked_arm_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::Arm {
                                allow_override: true,
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep startup blocked arm request",
            )
            .await;
            assert_eq!(blocked_arm_response.status(), StatusCode::CONFLICT);

            let resolved_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Dashboard,
                            command: RuntimeLifecycleCommand::ResolveReconnectReview {
                                decision: RuntimeReconnectDecision::ReattachBotManagement,
                                contract_id: None,
                                reason: Some("clear startup review for release sweep".to_owned()),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep startup resolve review request",
            )
            .await;
            assert_eq!(resolved_response.status(), StatusCode::OK);
            let resolved_body = resolved_response
                .into_body()
                .collect()
                .await
                .expect("body should collect")
                .to_bytes();
            let resolved_response: RuntimeLifecycleResponse =
                serde_json::from_slice(&resolved_body).expect("response json should parse");
            assert_eq!(
                resolved_response.message,
                "reconnect review resolved with reattach_bot_management"
            );
            assert!(!resolved_response.status.reconnect_review.required);

            let arm_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Cli,
                            command: RuntimeLifecycleCommand::Arm {
                                allow_override: true,
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep startup arm request",
            )
            .await;
            assert_eq!(arm_response.status(), StatusCode::OK);

            let cancel_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Dashboard,
                            command: RuntimeLifecycleCommand::CancelWorkingOrders {
                                reason: Some(
                                    "clear startup working orders for release sweep".to_owned(),
                                ),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep startup cancel request",
            )
            .await;
            assert_eq!(cancel_response.status(), StatusCode::OK);

            let close_response = request_with_timeout(
                app.clone(),
                Request::builder()
                    .method("POST")
                    .uri("/runtime/commands")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&RuntimeLifecycleRequest {
                            source: ManualCommandSource::Dashboard,
                            command: RuntimeLifecycleCommand::ClosePosition {
                                contract_id: None,
                                reason: Some(
                                    "flatten startup position for release sweep".to_owned(),
                                ),
                            },
                        })
                        .expect("request should serialize"),
                    ))
                    .expect("request should build"),
                "paper release sweep startup close request",
            )
            .await;
            assert_eq!(close_response.status(), StatusCode::OK);

            let cancel_orders = execution_api
                .cancel_orders
                .lock()
                .expect("execution mutex should not poison");
            assert_eq!(cancel_orders.len(), 1);
            assert_eq!(cancel_orders[0].context.account_id, 101);
            assert_eq!(cancel_orders[0].order_id, 8102);
            drop(cancel_orders);

            let liquidations = execution_api
                .liquidations
                .lock()
                .expect("execution mutex should not poison");
            assert_eq!(liquidations.len(), 1);
            assert_eq!(liquidations[0].context.account_id, 101);
            assert_eq!(liquidations[0].contract_id, 4444);
            drop(liquidations);

            let place_osos = execution_api
                .place_osos
                .lock()
                .expect("execution mutex should not poison");
            assert!(place_osos.is_empty());
            drop(place_osos);

            let history_after = request_with_timeout(
                app.clone(),
                Request::builder()
                    .uri("/history")
                    .body(Body::empty())
                    .expect("request should build"),
                "paper release sweep startup history after request",
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

            let final_status = request_with_timeout(
                app.clone(),
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request should build"),
                "paper release sweep startup final status request",
            )
            .await;
            assert_eq!(final_status.status(), StatusCode::OK);
            let final_status_body = final_status
                .into_body()
                .collect()
                .await
                .expect("body should collect")
                .to_bytes();
            let final_status: RuntimeStatusSnapshot =
                serde_json::from_slice(&final_status_body).expect("status json should parse");
            assert_eq!(final_status.mode, RuntimeMode::Paper);
            assert_eq!(final_status.arm_state, ArmState::Armed);
            assert!(!final_status.reconnect_review.required);

            let journal_records = journal.list().expect("journal should list records");
            assert!(journal_records.iter().any(|record| {
                record.action == "reconnect_review_resolved"
                    && record.payload["decision"].as_str() == Some("reattach_bot_management")
            }));
            assert_eq!(
                journal_records
                    .iter()
                    .filter(|record| record.action == "dispatch_succeeded")
                    .count(),
                2
            );

            let _ = fs::remove_file(strategy_path);
        }
    }

    #[tokio::test]
    async fn journal_route_returns_recent_persisted_records() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();

        for (event_id, action, occurred_at) in [
            ("evt-1", "intent_received", "2026-04-12T20:10:00Z"),
            ("evt-2", "decision", "2026-04-12T20:10:01Z"),
            ("evt-3", "dispatch_succeeded", "2026-04-12T20:10:02Z"),
        ] {
            journal
                .append(EventJournalRecord {
                    event_id: event_id.to_owned(),
                    category: "execution".to_owned(),
                    action: action.to_owned(),
                    source: ActionSource::Dashboard,
                    severity: tv_bot_core_types::EventSeverity::Info,
                    occurred_at: DateTime::parse_from_rfc3339(occurred_at)
                        .expect("timestamp should parse")
                        .with_timezone(&Utc),
                    payload: serde_json::json!({
                        "event_id": event_id,
                    }),
                })
                .expect("journal append should succeed");
        }

        let app = build_http_router(build_kernel_backed_state_with_manager(
            sample_session_manager().await,
            execution_api,
            journal,
            history,
            latency_collector,
            health_supervisor,
        ));

        let response = request_with_timeout(
            app,
            Request::builder()
                .uri("/journal")
                .body(Body::empty())
                .expect("request should build"),
            "journal route request",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let journal_snapshot: RuntimeJournalSnapshot =
            serde_json::from_slice(&body).expect("journal json should parse");

        assert_eq!(journal_snapshot.total_records, 3);
        assert_eq!(journal_snapshot.records.len(), 3);
        assert_eq!(journal_snapshot.records[0].event_id, "evt-3");
        assert_eq!(journal_snapshot.records[1].event_id, "evt-2");
        assert_eq!(journal_snapshot.records[2].event_id, "evt-1");
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
    async fn chart_config_route_reports_loaded_contract_and_supported_timeframes() {
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
            "chart config load strategy",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        set_test_market_data_snapshot(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
        )
        .await;

        let response = request_with_timeout(
            app,
            Request::builder()
                .uri("/chart/config")
                .body(Body::empty())
                .expect("request should build"),
            "chart config route",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let config: RuntimeChartConfigResponse =
            serde_json::from_slice(&body).expect("chart config json should parse");

        assert!(config.available);
        assert_eq!(
            config
                .instrument
                .as_ref()
                .and_then(|instrument| instrument.tradovate_symbol.as_deref()),
            Some("GCM2026")
        );
        assert_eq!(
            config
                .instrument
                .as_ref()
                .map(|instrument| instrument.databento_symbols.clone()),
            Some(vec!["GCM6".to_owned()])
        );
        assert_eq!(
            config.supported_timeframes,
            vec![
                Timeframe::OneSecond,
                Timeframe::OneMinute,
                Timeframe::FiveMinute
            ]
        );
        assert_eq!(config.default_timeframe, Some(Timeframe::OneSecond));
        assert!(!config.sample_data_active);
        assert_eq!(
            config.market_data_health,
            Some(tv_bot_market_data::MarketDataHealth::Healthy)
        );
    }

    #[tokio::test]
    async fn chart_snapshot_route_returns_sample_bars_when_market_data_is_unconfigured() {
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
            "chart sample snapshot load strategy",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        let response = request_with_timeout(
            app,
            Request::builder()
                .uri("/chart/snapshot?timeframe=1m&limit=60")
                .body(Body::empty())
                .expect("request should build"),
            "chart sample snapshot route",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let snapshot: RuntimeChartSnapshot =
            serde_json::from_slice(&body).expect("chart snapshot json should parse");

        assert_eq!(snapshot.timeframe, Timeframe::OneMinute);
        assert_eq!(snapshot.requested_limit, 60);
        assert_eq!(snapshot.bars.len(), 60);
        assert!(snapshot.config.available);
        assert!(snapshot.config.sample_data_active);
        assert!(snapshot.latest_price.is_some());
        assert!(snapshot.can_load_older_history);
        assert_eq!(snapshot.config.market_data_connection_state, None);
        assert_eq!(snapshot.config.market_data_health, None);
        assert!(
            snapshot
                .config
                .detail
                .contains("illustrative sample candles"),
            "expected sample-data detail, got: {}",
            snapshot.config.detail
        );
    }

    #[tokio::test]
    async fn chart_snapshot_route_returns_requested_bars_and_active_overlays() {
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
            "chart snapshot load strategy",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        let base_time = Utc
            .with_ymd_and_hms(2026, 4, 15, 14, 0, 0)
            .single()
            .expect("timestamp should be valid");
        set_test_market_data_snapshot_with_chart_bars(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
            BTreeMap::from([(
                Timeframe::OneMinute,
                vec![
                    sample_chart_bar(
                        Timeframe::OneMinute,
                        base_time,
                        238_400,
                        238_450,
                        238_350,
                        238_425,
                        12,
                    ),
                    sample_chart_bar(
                        Timeframe::OneMinute,
                        base_time + ChronoDuration::minutes(1),
                        238_425,
                        238_500,
                        238_400,
                        238_475,
                        18,
                    ),
                    sample_chart_bar(
                        Timeframe::OneMinute,
                        base_time + ChronoDuration::minutes(2),
                        238_475,
                        238_550,
                        238_450,
                        238_525,
                        21,
                    ),
                ],
            )]),
        )
        .await;

        let response = request_with_timeout(
            app,
            Request::builder()
                .uri("/chart/snapshot?timeframe=1m&limit=2")
                .body(Body::empty())
                .expect("request should build"),
            "chart snapshot route",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let snapshot: RuntimeChartSnapshot =
            serde_json::from_slice(&body).expect("chart snapshot json should parse");

        assert_eq!(snapshot.timeframe, Timeframe::OneMinute);
        assert_eq!(snapshot.requested_limit, 2);
        assert_eq!(snapshot.bars.len(), 2);
        assert_eq!(
            snapshot.bars[0].closed_at,
            base_time + ChronoDuration::minutes(1)
        );
        assert_eq!(
            snapshot.bars[1].closed_at,
            base_time + ChronoDuration::minutes(2)
        );
        assert_eq!(snapshot.latest_price, Some(Decimal::new(238_525, 2)));
        assert!(snapshot.can_load_older_history);
        assert_eq!(
            snapshot
                .active_position
                .as_ref()
                .map(|position| position.symbol.as_str()),
            Some("GCM2026")
        );
        assert_eq!(snapshot.working_orders.len(), 1);
        assert_eq!(snapshot.recent_fills.len(), 1);
        assert!(snapshot.config.available);
    }

    #[tokio::test]
    async fn chart_history_route_pages_older_buffered_bars() {
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
            "chart history load strategy",
        )
        .await;
        assert_eq!(load_response.status(), StatusCode::OK);

        let base_time = Utc
            .with_ymd_and_hms(2026, 4, 15, 14, 0, 0)
            .single()
            .expect("timestamp should be valid");
        set_test_market_data_snapshot_with_chart_bars(
            &state,
            sample_market_data_snapshot(tv_bot_market_data::MarketDataHealth::Healthy),
            None,
            BTreeMap::from([(
                Timeframe::OneMinute,
                vec![
                    sample_chart_bar(
                        Timeframe::OneMinute,
                        base_time,
                        238_400,
                        238_450,
                        238_350,
                        238_425,
                        12,
                    ),
                    sample_chart_bar(
                        Timeframe::OneMinute,
                        base_time + ChronoDuration::minutes(1),
                        238_425,
                        238_500,
                        238_400,
                        238_475,
                        18,
                    ),
                    sample_chart_bar(
                        Timeframe::OneMinute,
                        base_time + ChronoDuration::minutes(2),
                        238_475,
                        238_550,
                        238_450,
                        238_525,
                        21,
                    ),
                ],
            )]),
        )
        .await;

        let response = request_with_timeout(
            app,
            Request::builder()
                .uri(&format!(
                    "/chart/history?timeframe=1m&before={}&limit=1",
                    (base_time + ChronoDuration::minutes(2))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ))
                .body(Body::empty())
                .expect("request should build"),
            "chart history route",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let history_response: RuntimeChartHistoryResponse =
            serde_json::from_slice(&body).expect("chart history json should parse");

        assert_eq!(history_response.timeframe, Timeframe::OneMinute);
        assert_eq!(history_response.requested_limit, 1);
        assert_eq!(history_response.bars.len(), 1);
        assert_eq!(
            history_response.bars[0].closed_at,
            base_time + ChronoDuration::minutes(1)
        );
        assert!(history_response.can_load_older_history);
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
    async fn startup_review_matrix_blocks_arming_for_position_and_order_variants() {
        for (scenario_label, snapshot, expected_open_positions, expected_working_orders) in [
            (
                "startup mixed exposure",
                sync_snapshot_with_open_position(),
                1,
                1,
            ),
            (
                "startup position only",
                sync_snapshot_with_position_only(),
                1,
                0,
            ),
            (
                "startup working orders only",
                sync_snapshot_with_working_orders_only(),
                0,
                1,
            ),
        ] {
            assert_review_required_snapshot_blocks_arming_through_runtime_host(
                ReviewTriggerPhase::Startup,
                scenario_label,
                snapshot,
                expected_open_positions,
                expected_working_orders,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn reconnect_review_matrix_blocks_arming_for_position_and_order_variants() {
        for (scenario_label, snapshot, expected_open_positions, expected_working_orders) in [
            (
                "reconnect mixed exposure",
                sync_snapshot_with_open_position(),
                1,
                1,
            ),
            (
                "reconnect position only",
                sync_snapshot_with_position_only(),
                1,
                0,
            ),
            (
                "reconnect working orders only",
                sync_snapshot_with_working_orders_only(),
                0,
                1,
            ),
        ] {
            assert_review_required_snapshot_blocks_arming_through_runtime_host(
                ReviewTriggerPhase::Reconnect,
                scenario_label,
                snapshot,
                expected_open_positions,
                expected_working_orders,
            )
            .await;
        }
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
    async fn startup_review_blocks_new_paper_entry_until_operator_resolution_through_runtime_host()
    {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_startup_review_required_working_orders().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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
            "startup review load strategy request",
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
                "startup review set mode paper request",
                RuntimeLifecycleCommand::SetMode {
                    mode: RuntimeMode::Paper,
                },
            ),
            (
                "startup review mark warmup ready request",
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

        let status_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "startup review status request",
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
        assert_eq!(status_before.arm_state, ArmState::Disarmed);
        assert_eq!(
            status_before.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(status_before.reconnect_review.required);
        assert_eq!(
            status_before.reconnect_review.reason.as_deref(),
            Some("existing broker-side position or working orders detected at startup")
        );
        assert_eq!(status_before.reconnect_review.last_decision, None);
        assert_eq!(status_before.reconnect_review.open_position_count, 0);
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
            Some(0)
        );

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "startup review history before request",
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

        let blocked_arm_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::Arm {
                            allow_override: true,
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "startup review blocked arm request",
        )
        .await;

        assert_eq!(blocked_arm_response.status(), StatusCode::CONFLICT);
        let blocked_arm_body = blocked_arm_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let blocked_arm: RuntimeLifecycleResponse =
            serde_json::from_slice(&blocked_arm_body).expect("response json should parse");
        assert_eq!(blocked_arm.status_code, HttpStatusCode::Conflict);
        assert!(blocked_arm
            .message
            .contains("readiness report contains blocking issues"));
        assert!(blocked_arm.command_result.is_none());
        assert!(blocked_arm.status.reconnect_review.required);
        assert_eq!(blocked_arm.status.arm_state, ArmState::Disarmed);

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

        let resolved_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ResolveReconnectReview {
                            decision: RuntimeReconnectDecision::ReattachBotManagement,
                            contract_id: None,
                            reason: Some("resume paper trading after startup review".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "startup review resolution request",
        )
        .await;

        assert_eq!(resolved_response.status(), StatusCode::OK);
        let resolved_body = resolved_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let resolved_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&resolved_body).expect("response json should parse");
        assert_eq!(resolved_response.status_code, HttpStatusCode::Ok);
        assert_eq!(
            resolved_response.message,
            "reconnect review resolved with reattach_bot_management"
        );
        assert!(!resolved_response.status.reconnect_review.required);
        assert_eq!(resolved_response.status.arm_state, ArmState::Disarmed);
        assert_eq!(
            resolved_response.status.reconnect_review.last_decision,
            Some(RuntimeReconnectDecision::ReattachBotManagement)
        );
        assert_eq!(
            resolved_response
                .status
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.reconnect_count),
            Some(0)
        );

        let armed_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Cli,
                        command: RuntimeLifecycleCommand::Arm {
                            allow_override: true,
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "startup review arm after resolution request",
        )
        .await;

        assert_eq!(armed_response.status(), StatusCode::OK);
        let armed_body = armed_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let armed_response: RuntimeLifecycleResponse =
            serde_json::from_slice(&armed_body).expect("response json should parse");
        assert_eq!(armed_response.status_code, HttpStatusCode::Ok);
        assert_eq!(
            armed_response.message,
            "runtime armed with temporary override"
        );
        assert_eq!(armed_response.status.arm_state, ArmState::Armed);
        assert!(!armed_response.status.reconnect_review.required);

        let allowed_entry_response = request_with_timeout(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/runtime/commands")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeLifecycleRequest {
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ManualEntry {
                            side: tv_bot_core_types::TradeSide::Buy,
                            quantity: 1,
                            tick_size: Decimal::new(10, 1),
                            entry_reference_price: Decimal::new(238_510, 2),
                            tick_value_usd: Some(Decimal::new(10, 0)),
                            reason: Some("startup review paper entry".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "startup review allowed manual entry request",
        )
        .await;

        assert_eq!(allowed_entry_response.status(), StatusCode::OK);
        let allowed_entry_body = allowed_entry_response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let allowed_entry: RuntimeLifecycleResponse =
            serde_json::from_slice(&allowed_entry_body).expect("response json should parse");
        assert_eq!(allowed_entry.status_code, HttpStatusCode::Ok);
        assert_eq!(allowed_entry.message, "manual entry command dispatched");
        let command_result = allowed_entry
            .command_result
            .expect("manual entry should return a command result after review");
        assert_eq!(command_result.status, ControlApiCommandStatus::Executed);
        assert_eq!(command_result.risk_status, RiskDecisionStatus::Accepted);
        assert!(command_result.dispatch_performed);

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
            "startup review history after request",
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

        let journal_records = journal.list().expect("journal should list records");
        let journal_actions = journal_records
            .iter()
            .map(|record| record.action.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            journal_actions,
            vec![
                "reconnect_review_resolved".to_owned(),
                "intent_received".to_owned(),
                "decision".to_owned(),
                "dispatch_succeeded".to_owned(),
            ]
        );
        let reconnect_resolution = journal_records
            .iter()
            .find(|record| record.action == "reconnect_review_resolved")
            .expect("startup review resolution should be journaled");
        assert_eq!(reconnect_resolution.category, "broker");
        assert_eq!(reconnect_resolution.source, ActionSource::Dashboard);
        assert_eq!(
            reconnect_resolution.payload["decision"].as_str(),
            Some("reattach_bot_management")
        );
        assert_eq!(
            reconnect_resolution.payload["reason"].as_str(),
            Some("resume paper trading after startup review")
        );
        assert_eq!(
            reconnect_resolution.payload["open_position_count"].as_u64(),
            Some(0)
        );
        assert_eq!(
            reconnect_resolution.payload["working_order_count"].as_u64(),
            Some(1)
        );

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn startup_review_after_existing_position_can_leave_broker_protected_through_runtime_host(
    ) {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_startup_review_required().await,
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
            "startup review leave set mode paper request",
        )
        .await;
        assert_eq!(set_mode_response.status(), StatusCode::OK);

        let status_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .expect("request should build"),
            "startup review leave status request",
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
            Some("existing broker-side position or working orders detected at startup")
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
            Some(0)
        );

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "startup review leave history before request",
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
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ResolveReconnectReview {
                            decision: RuntimeReconnectDecision::LeaveBrokerProtected,
                            contract_id: None,
                            reason: Some("leave paper protections in place at startup".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "startup review leave protected request",
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
            "reconnect review resolved with leave_broker_protected"
        );
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(
            lifecycle_response.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(!lifecycle_response.status.reconnect_review.required);
        assert_eq!(
            lifecycle_response.status.reconnect_review.last_decision,
            Some(RuntimeReconnectDecision::LeaveBrokerProtected)
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
            Some(0)
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
            "startup review leave history after request",
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
            .expect("startup leave resolution should be journaled");
        assert_eq!(reconnect_resolution.category, "broker");
        assert_eq!(reconnect_resolution.source, ActionSource::Dashboard);
        assert_eq!(
            reconnect_resolution.payload["decision"].as_str(),
            Some("leave_broker_protected")
        );
        assert_eq!(
            reconnect_resolution.payload["reason"].as_str(),
            Some("leave paper protections in place at startup")
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
    async fn startup_review_close_position_dispatches_flatten_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_contract_startup_review_required().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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
                "startup review close setup request",
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
            "startup review close status request",
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
            Some("existing broker-side position or working orders detected at startup")
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
            Some(0)
        );

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "startup review close history before request",
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
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ResolveReconnectReview {
                            decision: RuntimeReconnectDecision::ClosePosition,
                            contract_id: None,
                            reason: Some("close paper position at startup".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "startup review close request",
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
            "reconnect close command dispatched"
        );
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(
            lifecycle_response.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(lifecycle_response.status.reconnect_review.required);
        assert_eq!(
            lifecycle_response.status.reconnect_review.last_decision,
            None
        );
        assert_eq!(
            lifecycle_response
                .status
                .broker_status
                .as_ref()
                .map(|snapshot| snapshot.reconnect_count),
            Some(0)
        );
        let command_result = lifecycle_response
            .command_result
            .expect("startup close should return a command result");
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

        let history_after = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "startup review close history after request",
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

        let journal_records = journal.list().expect("journal should list records");
        let journal_actions = journal_records
            .iter()
            .map(|record| record.action.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            journal_actions,
            vec![
                "reconnect_review_close_requested".to_owned(),
                "intent_received".to_owned(),
                "decision".to_owned(),
                "dispatch_succeeded".to_owned(),
            ]
        );
        let reconnect_close = journal_records
            .iter()
            .find(|record| record.action == "reconnect_review_close_requested")
            .expect("startup close request should be journaled");
        assert_eq!(reconnect_close.category, "broker");
        assert_eq!(reconnect_close.source, ActionSource::Dashboard);
        assert_eq!(
            reconnect_close.payload["reason"].as_str(),
            Some("close paper position at startup")
        );
        assert_eq!(reconnect_close.payload["contract_id"].as_i64(), Some(4444));

        let _ = fs::remove_file(strategy_path);
    }

    #[tokio::test]
    async fn paper_reconnect_review_after_disconnect_can_leave_broker_protected_through_runtime_host(
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
            "paper reconnect leave status request",
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

        let history_before = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper reconnect leave history before request",
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
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ResolveReconnectReview {
                            decision: RuntimeReconnectDecision::LeaveBrokerProtected,
                            contract_id: None,
                            reason: Some(
                                "leave paper protections in place after reconnect".to_owned(),
                            ),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper reconnect leave protected request",
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
            "reconnect review resolved with leave_broker_protected"
        );
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(
            lifecycle_response.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(!lifecycle_response.status.reconnect_review.required);
        assert_eq!(
            lifecycle_response.status.reconnect_review.last_decision,
            Some(RuntimeReconnectDecision::LeaveBrokerProtected)
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
            "paper reconnect leave history after request",
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
        assert_eq!(reconnect_resolution.source, ActionSource::Dashboard);
        assert_eq!(
            reconnect_resolution.payload["decision"].as_str(),
            Some("leave_broker_protected")
        );
        assert_eq!(
            reconnect_resolution.payload["reason"].as_str(),
            Some("leave paper protections in place after reconnect")
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
    async fn paper_reconnect_review_close_position_dispatches_flatten_through_runtime_host() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let execution_api = TestExecutionApi::default();
        let journal = InMemoryJournal::new();
        let state = build_kernel_backed_state_with_manager(
            sample_session_manager_with_contract_reconnect_review_required().await,
            execution_api.clone(),
            journal.clone(),
            history.clone(),
            latency_collector.clone(),
            health_supervisor.clone(),
        );
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
                "paper reconnect close setup request",
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
            "paper reconnect close status request",
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
            "paper reconnect close history before request",
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
                        source: ManualCommandSource::Dashboard,
                        command: RuntimeLifecycleCommand::ResolveReconnectReview {
                            decision: RuntimeReconnectDecision::ClosePosition,
                            contract_id: None,
                            reason: Some("close paper position after reconnect".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "paper reconnect close request",
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
            "reconnect close command dispatched"
        );
        assert_eq!(lifecycle_response.status.mode, RuntimeMode::Paper);
        assert_eq!(
            lifecycle_response.status.current_account_name.as_deref(),
            Some("paper-primary")
        );
        assert!(lifecycle_response.status.reconnect_review.required);
        assert_eq!(
            lifecycle_response.status.reconnect_review.last_decision,
            None
        );
        let command_result = lifecycle_response
            .command_result
            .expect("reconnect close should return a command result");
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

        let history_after = request_with_timeout(
            app.clone(),
            Request::builder()
                .uri("/history")
                .body(Body::empty())
                .expect("request should build"),
            "paper reconnect close history after request",
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

        let journal_records = journal.list().expect("journal should list records");
        let journal_actions = journal_records
            .iter()
            .map(|record| record.action.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            journal_actions,
            vec![
                "reconnect_review_close_requested".to_owned(),
                "intent_received".to_owned(),
                "decision".to_owned(),
                "dispatch_succeeded".to_owned(),
            ]
        );
        let reconnect_close = journal_records
            .iter()
            .find(|record| record.action == "reconnect_review_close_requested")
            .expect("reconnect close request should be journaled");
        assert_eq!(reconnect_close.category, "broker");
        assert_eq!(reconnect_close.source, ActionSource::Dashboard);
        assert_eq!(
            reconnect_close.payload["reason"].as_str(),
            Some("close paper position after reconnect")
        );
        assert_eq!(reconnect_close.payload["contract_id"].as_i64(), Some(4444));

        let _ = fs::remove_file(strategy_path);
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

    #[tokio::test]
    async fn settings_route_reports_session_only_when_runtime_has_no_config_file_path() {
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

        let response = request_with_timeout(
            app,
            Request::builder()
                .method("GET")
                .uri("/settings")
                .body(Body::empty())
                .expect("request should build"),
            "settings route",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let settings: RuntimeSettingsSnapshot =
            serde_json::from_slice(&body).expect("settings json should parse");

        assert_eq!(
            settings.persistence_mode,
            RuntimeSettingsPersistenceMode::SessionOnly
        );
        assert_eq!(settings.config_file_path, None);
        assert!(settings.restart_required);
        assert!(settings.detail.contains("session only"));
    }

    #[tokio::test]
    async fn updating_settings_persists_to_runtime_config_file() {
        let history = test_history();
        let latency_collector = test_latency_collector();
        let health_supervisor = test_health_supervisor();
        let config_path = temp_config_path();
        fs::write(
            &config_path,
            r#"
                [runtime]
                startup_mode = "observation"
                default_strategy_path = "strategies/sample.md"
                allow_sqlite_fallback = false

                [broker]
                paper_account_name = "paper-primary"
                live_account_name = "live-primary"

                [control_api]
                http_bind = "127.0.0.1:8080"
                websocket_bind = "127.0.0.1:8081"
            "#,
        )
        .expect("config file should write");

        let app = build_http_router(test_state_with_config_path(
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
            config_path.clone(),
        ));

        let response = request_with_timeout(
            app,
            Request::builder()
                .method("POST")
                .uri("/settings")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&RuntimeSettingsUpdateRequest {
                        source: ManualCommandSource::Dashboard,
                        settings: RuntimeEditableSettings {
                            startup_mode: RuntimeMode::Paper,
                            default_strategy_path: Some(PathBuf::from(
                                "strategies/uploads/next-run.md",
                            )),
                            allow_sqlite_fallback: true,
                            paper_account_name: Some("paper-secondary".to_owned()),
                            live_account_name: Some("live-ops".to_owned()),
                        },
                    })
                    .expect("request should serialize"),
                ))
                .expect("request should build"),
            "settings update",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let settings_response: RuntimeSettingsUpdateResponse =
            serde_json::from_slice(&body).expect("settings response json should parse");

        assert_eq!(
            settings_response.message,
            "saved runtime settings for the next restart"
        );
        assert_eq!(
            settings_response.settings.persistence_mode,
            RuntimeSettingsPersistenceMode::ConfigFile
        );
        assert_eq!(
            settings_response.settings.config_file_path,
            Some(config_path.clone())
        );
        assert!(matches!(
            settings_response.settings.editable.startup_mode,
            RuntimeMode::Paper
        ));
        assert_eq!(
            settings_response.settings.editable.default_strategy_path,
            Some(PathBuf::from("strategies/uploads/next-run.md"))
        );
        assert!(settings_response.settings.editable.allow_sqlite_fallback);
        assert_eq!(
            settings_response
                .settings
                .editable
                .paper_account_name
                .as_deref(),
            Some("paper-secondary")
        );
        assert_eq!(
            settings_response
                .settings
                .editable
                .live_account_name
                .as_deref(),
            Some("live-ops")
        );

        let persisted = AppConfig::load(Some(&config_path), &MapEnvironment::default())
            .expect("persisted config should reload");
        assert!(matches!(persisted.runtime.startup_mode, RuntimeMode::Paper));
        assert_eq!(
            persisted.runtime.default_strategy_path,
            Some(PathBuf::from("strategies/uploads/next-run.md"))
        );
        assert!(persisted.runtime.allow_sqlite_fallback);
        assert_eq!(
            persisted.broker.paper_account_name.as_deref(),
            Some("paper-secondary")
        );
        assert_eq!(
            persisted.broker.live_account_name.as_deref(),
            Some("live-ops")
        );

        let _ = fs::remove_file(config_path);
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
