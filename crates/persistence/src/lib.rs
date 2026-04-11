//! Postgres-first persistence contracts, backend selection, and durable adapters.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use postgres::{Client as PostgresClient, NoTls};
use rusqlite::{params, Connection as SqliteConnection};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tv_bot_config::AppConfig;
use tv_bot_core_types::{
    EventJournalRecord, FillRecord, OrderRecord, PnlSnapshotRecord, PositionRecord,
    StrategyRunRecord, SystemHealthSnapshot, TradePathLatencyRecord, TradeSummaryRecord,
};

pub const MODULE_STATUS: &str = "phase_6_runtime_backends";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceStorageMode {
    Unconfigured,
    PrimaryConfigured,
    SqliteFallbackOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistencePlan {
    pub mode: PersistenceStorageMode,
    pub primary_configured: bool,
    pub sqlite_fallback_enabled: bool,
    pub sqlite_path: PathBuf,
    pub allow_runtime_fallback: bool,
    pub detail: String,
}

impl PersistencePlan {
    pub fn from_config(config: &AppConfig) -> Self {
        let primary_configured = config
            .persistence
            .primary_url
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        let sqlite_fallback_enabled = config.persistence.sqlite_fallback.enabled;
        let allow_runtime_fallback = config.runtime.allow_sqlite_fallback;

        let (mode, detail) = if primary_configured {
            (
                PersistenceStorageMode::PrimaryConfigured,
                if sqlite_fallback_enabled {
                    "primary Postgres persistence is configured, but durable writes are not yet active in the runtime host; SQLite fallback is also configured".to_owned()
                } else {
                    "primary Postgres persistence is configured, but durable writes are not yet active in the runtime host".to_owned()
                },
            )
        } else if sqlite_fallback_enabled {
            (
                PersistenceStorageMode::SqliteFallbackOnly,
                "SQLite fallback is configured without a primary Postgres backend".to_owned(),
            )
        } else {
            (
                PersistenceStorageMode::Unconfigured,
                "no primary Postgres backend or SQLite fallback is configured".to_owned(),
            )
        };

        Self {
            mode,
            primary_configured,
            sqlite_fallback_enabled,
            sqlite_path: config.persistence.sqlite_fallback.path.clone(),
            allow_runtime_fallback,
            detail,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceBackendKind {
    InMemory,
    Sqlite,
    Postgres,
}

impl PersistenceBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in_memory",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistenceRuntimeSelection {
    pub plan: PersistencePlan,
    pub active_backend: PersistenceBackendKind,
    pub durable: bool,
    pub fallback_activated: bool,
    pub detail: String,
}

pub trait EventJournalStore: Send + Sync {
    fn append_event(&self, record: EventJournalRecord) -> Result<(), PersistenceError>;
    fn list_events(&self) -> Result<Vec<EventJournalRecord>, PersistenceError>;
}

pub trait TradeLatencyStore: Send + Sync {
    fn append_trade_latency(&self, record: TradePathLatencyRecord) -> Result<(), PersistenceError>;
    fn list_trade_latencies(&self) -> Result<Vec<TradePathLatencyRecord>, PersistenceError>;
}

pub trait SystemHealthStore: Send + Sync {
    fn append_system_health(&self, snapshot: SystemHealthSnapshot) -> Result<(), PersistenceError>;
    fn list_system_health(&self) -> Result<Vec<SystemHealthSnapshot>, PersistenceError>;
}

pub trait StrategyRunStore: Send + Sync {
    fn append_strategy_run(&self, record: StrategyRunRecord) -> Result<(), PersistenceError>;
    fn list_strategy_runs(&self) -> Result<Vec<StrategyRunRecord>, PersistenceError>;
}

pub trait OrderStore: Send + Sync {
    fn append_order(&self, record: OrderRecord) -> Result<(), PersistenceError>;
    fn list_orders(&self) -> Result<Vec<OrderRecord>, PersistenceError>;
}

pub trait FillStore: Send + Sync {
    fn append_fill(&self, record: FillRecord) -> Result<(), PersistenceError>;
    fn list_fills(&self) -> Result<Vec<FillRecord>, PersistenceError>;
}

pub trait PositionStore: Send + Sync {
    fn append_position(&self, record: PositionRecord) -> Result<(), PersistenceError>;
    fn list_positions(&self) -> Result<Vec<PositionRecord>, PersistenceError>;
}

pub trait PnlSnapshotStore: Send + Sync {
    fn append_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), PersistenceError>;
    fn list_pnl_snapshots(&self) -> Result<Vec<PnlSnapshotRecord>, PersistenceError>;
}

pub trait TradeSummaryStore: Send + Sync {
    fn append_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), PersistenceError>;
    fn list_trade_summaries(&self) -> Result<Vec<TradeSummaryRecord>, PersistenceError>;
}

#[derive(Clone, Debug)]
pub enum PersistenceBackend {
    InMemory(InMemoryPersistence),
    Sqlite(SqlitePersistence),
    Postgres(PostgresPersistence),
}

impl PersistenceBackend {
    fn kind(&self) -> PersistenceBackendKind {
        match self {
            Self::InMemory(_) => PersistenceBackendKind::InMemory,
            Self::Sqlite(_) => PersistenceBackendKind::Sqlite,
            Self::Postgres(_) => PersistenceBackendKind::Postgres,
        }
    }
}

impl EventJournalStore for PersistenceBackend {
    fn append_event(&self, record: EventJournalRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_event(record),
            Self::Sqlite(store) => store.append_event(record),
            Self::Postgres(store) => store.append_event(record),
        }
    }

    fn list_events(&self) -> Result<Vec<EventJournalRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_events(),
            Self::Sqlite(store) => store.list_events(),
            Self::Postgres(store) => store.list_events(),
        }
    }
}

impl TradeLatencyStore for PersistenceBackend {
    fn append_trade_latency(&self, record: TradePathLatencyRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_trade_latency(record),
            Self::Sqlite(store) => store.append_trade_latency(record),
            Self::Postgres(store) => store.append_trade_latency(record),
        }
    }

    fn list_trade_latencies(&self) -> Result<Vec<TradePathLatencyRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_trade_latencies(),
            Self::Sqlite(store) => store.list_trade_latencies(),
            Self::Postgres(store) => store.list_trade_latencies(),
        }
    }
}

impl SystemHealthStore for PersistenceBackend {
    fn append_system_health(&self, snapshot: SystemHealthSnapshot) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_system_health(snapshot),
            Self::Sqlite(store) => store.append_system_health(snapshot),
            Self::Postgres(store) => store.append_system_health(snapshot),
        }
    }

    fn list_system_health(&self) -> Result<Vec<SystemHealthSnapshot>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_system_health(),
            Self::Sqlite(store) => store.list_system_health(),
            Self::Postgres(store) => store.list_system_health(),
        }
    }
}

impl StrategyRunStore for PersistenceBackend {
    fn append_strategy_run(&self, record: StrategyRunRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_strategy_run(record),
            Self::Sqlite(store) => store.append_strategy_run(record),
            Self::Postgres(store) => store.append_strategy_run(record),
        }
    }

    fn list_strategy_runs(&self) -> Result<Vec<StrategyRunRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_strategy_runs(),
            Self::Sqlite(store) => store.list_strategy_runs(),
            Self::Postgres(store) => store.list_strategy_runs(),
        }
    }
}

impl OrderStore for PersistenceBackend {
    fn append_order(&self, record: OrderRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_order(record),
            Self::Sqlite(store) => store.append_order(record),
            Self::Postgres(store) => store.append_order(record),
        }
    }

    fn list_orders(&self) -> Result<Vec<OrderRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_orders(),
            Self::Sqlite(store) => store.list_orders(),
            Self::Postgres(store) => store.list_orders(),
        }
    }
}

impl FillStore for PersistenceBackend {
    fn append_fill(&self, record: FillRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_fill(record),
            Self::Sqlite(store) => store.append_fill(record),
            Self::Postgres(store) => store.append_fill(record),
        }
    }

    fn list_fills(&self) -> Result<Vec<FillRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_fills(),
            Self::Sqlite(store) => store.list_fills(),
            Self::Postgres(store) => store.list_fills(),
        }
    }
}

impl PositionStore for PersistenceBackend {
    fn append_position(&self, record: PositionRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_position(record),
            Self::Sqlite(store) => store.append_position(record),
            Self::Postgres(store) => store.append_position(record),
        }
    }

    fn list_positions(&self) -> Result<Vec<PositionRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_positions(),
            Self::Sqlite(store) => store.list_positions(),
            Self::Postgres(store) => store.list_positions(),
        }
    }
}

impl PnlSnapshotStore for PersistenceBackend {
    fn append_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_pnl_snapshot(record),
            Self::Sqlite(store) => store.append_pnl_snapshot(record),
            Self::Postgres(store) => store.append_pnl_snapshot(record),
        }
    }

    fn list_pnl_snapshots(&self) -> Result<Vec<PnlSnapshotRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_pnl_snapshots(),
            Self::Sqlite(store) => store.list_pnl_snapshots(),
            Self::Postgres(store) => store.list_pnl_snapshots(),
        }
    }
}

impl TradeSummaryStore for PersistenceBackend {
    fn append_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), PersistenceError> {
        match self {
            Self::InMemory(store) => store.append_trade_summary(record),
            Self::Sqlite(store) => store.append_trade_summary(record),
            Self::Postgres(store) => store.append_trade_summary(record),
        }
    }

    fn list_trade_summaries(&self) -> Result<Vec<TradeSummaryRecord>, PersistenceError> {
        match self {
            Self::InMemory(store) => store.list_trade_summaries(),
            Self::Sqlite(store) => store.list_trade_summaries(),
            Self::Postgres(store) => store.list_trade_summaries(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RuntimePersistence {
    backend: Arc<PersistenceBackend>,
    selection: PersistenceRuntimeSelection,
}

impl RuntimePersistence {
    pub fn open(config: &AppConfig) -> Self {
        let plan = PersistencePlan::from_config(config);

        if let Some(primary_url) = config
            .persistence
            .primary_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match PostgresPersistence::connect(primary_url.to_owned()) {
                Ok(backend) => {
                    let detail = if plan.sqlite_fallback_enabled {
                        format!(
                            "primary Postgres persistence is active; SQLite fallback is configured at `{}`",
                            plan.sqlite_path.display()
                        )
                    } else {
                        "primary Postgres persistence is active".to_owned()
                    };
                    return Self::from_backend(
                        plan,
                        PersistenceBackend::Postgres(backend),
                        true,
                        false,
                        detail,
                    );
                }
                Err(primary_error) => {
                    if plan.sqlite_fallback_enabled {
                        match SqlitePersistence::open(plan.sqlite_path.clone()) {
                            Ok(backend) => {
                                return Self::from_backend(
                                    plan,
                                    PersistenceBackend::Sqlite(backend),
                                    true,
                                    true,
                                    format!(
                                        "primary Postgres persistence is unavailable: {primary_error}; SQLite fallback is active at `{}`",
                                        config.persistence.sqlite_fallback.path.display()
                                    ),
                                );
                            }
                            Err(sqlite_error) => {
                                return Self::from_backend(
                                    plan,
                                    PersistenceBackend::InMemory(InMemoryPersistence::new()),
                                    false,
                                    false,
                                    format!(
                                        "primary Postgres persistence is unavailable: {primary_error}; SQLite fallback also failed: {sqlite_error}; runtime is using in-memory persistence only"
                                    ),
                                );
                            }
                        }
                    }

                    return Self::from_backend(
                        plan,
                        PersistenceBackend::InMemory(InMemoryPersistence::new()),
                        false,
                        false,
                        format!(
                            "primary Postgres persistence is unavailable: {primary_error}; no SQLite fallback is configured, so runtime is using in-memory persistence only"
                        ),
                    );
                }
            }
        }

        if plan.sqlite_fallback_enabled {
            match SqlitePersistence::open(plan.sqlite_path.clone()) {
                Ok(backend) => {
                    return Self::from_backend(
                        plan,
                        PersistenceBackend::Sqlite(backend),
                        true,
                        false,
                        format!(
                            "SQLite fallback-only persistence is active at `{}`",
                            config.persistence.sqlite_fallback.path.display()
                        ),
                    );
                }
                Err(sqlite_error) => {
                    return Self::from_backend(
                        plan,
                        PersistenceBackend::InMemory(InMemoryPersistence::new()),
                        false,
                        false,
                        format!(
                            "SQLite fallback backend is unavailable: {sqlite_error}; runtime is using in-memory persistence only"
                        ),
                    );
                }
            }
        }

        Self::from_backend(
            plan,
            PersistenceBackend::InMemory(InMemoryPersistence::new()),
            false,
            false,
            "no durable persistence backend is configured; runtime is using in-memory persistence only"
                .to_owned(),
        )
    }

    fn from_backend(
        plan: PersistencePlan,
        backend: PersistenceBackend,
        durable: bool,
        fallback_activated: bool,
        detail: String,
    ) -> Self {
        let kind = backend.kind();
        Self {
            backend: Arc::new(backend),
            selection: PersistenceRuntimeSelection {
                plan,
                active_backend: kind,
                durable,
                fallback_activated,
                detail,
            },
        }
    }

    pub fn selection(&self) -> &PersistenceRuntimeSelection {
        &self.selection
    }

    pub fn event_store(&self) -> Arc<dyn EventJournalStore> {
        self.backend.clone()
    }

    pub fn trade_latency_store(&self) -> Arc<dyn TradeLatencyStore> {
        self.backend.clone()
    }

    pub fn system_health_store(&self) -> Arc<dyn SystemHealthStore> {
        self.backend.clone()
    }

    pub fn strategy_run_store(&self) -> Arc<dyn StrategyRunStore> {
        self.backend.clone()
    }

    pub fn order_store(&self) -> Arc<dyn OrderStore> {
        self.backend.clone()
    }

    pub fn fill_store(&self) -> Arc<dyn FillStore> {
        self.backend.clone()
    }

    pub fn position_store(&self) -> Arc<dyn PositionStore> {
        self.backend.clone()
    }

    pub fn pnl_snapshot_store(&self) -> Arc<dyn PnlSnapshotStore> {
        self.backend.clone()
    }

    pub fn trade_summary_store(&self) -> Arc<dyn TradeSummaryStore> {
        self.backend.clone()
    }
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryPersistence {
    events: Arc<Mutex<Vec<EventJournalRecord>>>,
    trade_latencies: Arc<Mutex<Vec<TradePathLatencyRecord>>>,
    system_health: Arc<Mutex<Vec<SystemHealthSnapshot>>>,
    strategy_runs: Arc<Mutex<Vec<StrategyRunRecord>>>,
    orders: Arc<Mutex<Vec<OrderRecord>>>,
    fills: Arc<Mutex<Vec<FillRecord>>>,
    positions: Arc<Mutex<Vec<PositionRecord>>>,
    pnl_snapshots: Arc<Mutex<Vec<PnlSnapshotRecord>>>,
    trade_summaries: Arc<Mutex<Vec<TradeSummaryRecord>>>,
}

impl InMemoryPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&self) -> Result<(), PersistenceError> {
        self.events
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.trade_latencies
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.system_health
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.strategy_runs
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.orders
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.fills
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.positions
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.pnl_snapshots
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        self.trade_summaries
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clear();
        Ok(())
    }
}

impl EventJournalStore for InMemoryPersistence {
    fn append_event(&self, record: EventJournalRecord) -> Result<(), PersistenceError> {
        self.events
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_events(&self) -> Result<Vec<EventJournalRecord>, PersistenceError> {
        Ok(self
            .events
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl TradeLatencyStore for InMemoryPersistence {
    fn append_trade_latency(&self, record: TradePathLatencyRecord) -> Result<(), PersistenceError> {
        self.trade_latencies
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_trade_latencies(&self) -> Result<Vec<TradePathLatencyRecord>, PersistenceError> {
        Ok(self
            .trade_latencies
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl SystemHealthStore for InMemoryPersistence {
    fn append_system_health(&self, snapshot: SystemHealthSnapshot) -> Result<(), PersistenceError> {
        self.system_health
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(snapshot);
        Ok(())
    }

    fn list_system_health(&self) -> Result<Vec<SystemHealthSnapshot>, PersistenceError> {
        Ok(self
            .system_health
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl StrategyRunStore for InMemoryPersistence {
    fn append_strategy_run(&self, record: StrategyRunRecord) -> Result<(), PersistenceError> {
        self.strategy_runs
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_strategy_runs(&self) -> Result<Vec<StrategyRunRecord>, PersistenceError> {
        Ok(self
            .strategy_runs
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl OrderStore for InMemoryPersistence {
    fn append_order(&self, record: OrderRecord) -> Result<(), PersistenceError> {
        self.orders
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_orders(&self) -> Result<Vec<OrderRecord>, PersistenceError> {
        Ok(self
            .orders
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl FillStore for InMemoryPersistence {
    fn append_fill(&self, record: FillRecord) -> Result<(), PersistenceError> {
        self.fills
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_fills(&self) -> Result<Vec<FillRecord>, PersistenceError> {
        Ok(self
            .fills
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl PositionStore for InMemoryPersistence {
    fn append_position(&self, record: PositionRecord) -> Result<(), PersistenceError> {
        self.positions
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_positions(&self) -> Result<Vec<PositionRecord>, PersistenceError> {
        Ok(self
            .positions
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl PnlSnapshotStore for InMemoryPersistence {
    fn append_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), PersistenceError> {
        self.pnl_snapshots
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_pnl_snapshots(&self) -> Result<Vec<PnlSnapshotRecord>, PersistenceError> {
        Ok(self
            .pnl_snapshots
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

impl TradeSummaryStore for InMemoryPersistence {
    fn append_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), PersistenceError> {
        self.trade_summaries
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .push(record);
        Ok(())
    }

    fn list_trade_summaries(&self) -> Result<Vec<TradeSummaryRecord>, PersistenceError> {
        Ok(self
            .trade_summaries
            .lock()
            .map_err(|_| PersistenceError::Poisoned)?
            .clone())
    }
}

#[derive(Clone, Debug)]
pub struct SqlitePersistence {
    path: PathBuf,
}

impl SqlitePersistence {
    pub fn open(path: PathBuf) -> Result<Self, PersistenceError> {
        ensure_parent_dir(&path)?;
        let connection =
            SqliteConnection::open(&path).map_err(|source| PersistenceError::Sqlite {
                operation: "open",
                source,
            })?;
        init_sqlite_schema(&connection)?;
        Ok(Self { path })
    }

    fn with_connection<T>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&SqliteConnection) -> Result<T, PersistenceError>,
    ) -> Result<T, PersistenceError> {
        let connection = SqliteConnection::open(&self.path)
            .map_err(|source| PersistenceError::Sqlite { operation, source })?;
        init_sqlite_schema(&connection)?;
        f(&connection)
    }
}

impl EventJournalStore for SqlitePersistence {
    fn append_event(&self, record: EventJournalRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_event", |connection| {
            let payload_json = serde_json::to_string(&record.payload)
                .map_err(|source| PersistenceError::Serialization { source })?;
            connection
                .execute(
                    r#"
                    INSERT OR REPLACE INTO event_journal (
                        event_id,
                        category,
                        action,
                        source,
                        severity,
                        occurred_at,
                        payload_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    "#,
                    params![
                        record.event_id,
                        record.category,
                        record.action,
                        enum_to_storage(&record.source)?,
                        enum_to_storage(&record.severity)?,
                        timestamp_to_storage(&record.occurred_at),
                        payload_json,
                    ],
                )
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "append_event",
                    source,
                })?;
            Ok(())
        })
    }

    fn list_events(&self) -> Result<Vec<EventJournalRecord>, PersistenceError> {
        self.with_connection("list_events", |connection| {
            let mut statement = connection
                .prepare(
                    r#"
                    SELECT event_id, category, action, source, severity, occurred_at, payload_json
                    FROM event_journal
                    ORDER BY occurred_at ASC, event_id ASC
                    "#,
                )
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "list_events",
                    source,
                })?;
            let rows = statement
                .query_map([], |row| {
                    let source: String = row.get(3)?;
                    let severity: String = row.get(4)?;
                    let occurred_at: String = row.get(5)?;
                    let payload_json: String = row.get(6)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        source,
                        severity,
                        occurred_at,
                        payload_json,
                    ))
                })
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "list_events",
                    source,
                })?;

            rows.into_iter()
                .map(|row| {
                    let (event_id, category, action, source, severity, occurred_at, payload_json) =
                        row.map_err(|source| PersistenceError::Sqlite {
                            operation: "list_events",
                            source,
                        })?;
                    Ok(EventJournalRecord {
                        event_id,
                        category,
                        action,
                        source: enum_from_storage(&source)?,
                        severity: enum_from_storage(&severity)?,
                        occurred_at: timestamp_from_storage("occurred_at", &occurred_at)?,
                        payload: serde_json::from_str(&payload_json)
                            .map_err(|source| PersistenceError::Serialization { source })?,
                    })
                })
                .collect()
        })
    }
}

impl TradeLatencyStore for SqlitePersistence {
    fn append_trade_latency(&self, record: TradePathLatencyRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_trade_latency", |connection| {
            let timestamps_json = serde_json::to_string(&record.timestamps)
                .map_err(|source| PersistenceError::Serialization { source })?;
            let latency_json = serde_json::to_string(&record.latency)
                .map_err(|source| PersistenceError::Serialization { source })?;
            connection
                .execute(
                    r#"
                    INSERT OR REPLACE INTO trade_path_latency (
                        action_id,
                        strategy_id,
                        recorded_at,
                        timestamps_json,
                        latency_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                    params![
                        record.action_id,
                        record.strategy_id,
                        timestamp_to_storage(&record.recorded_at),
                        timestamps_json,
                        latency_json,
                    ],
                )
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "append_trade_latency",
                    source,
                })?;
            Ok(())
        })
    }

    fn list_trade_latencies(&self) -> Result<Vec<TradePathLatencyRecord>, PersistenceError> {
        self.with_connection("list_trade_latencies", |connection| {
            let mut statement = connection
                .prepare(
                    r#"
                    SELECT action_id, strategy_id, recorded_at, timestamps_json, latency_json
                    FROM trade_path_latency
                    ORDER BY recorded_at ASC, action_id ASC
                    "#,
                )
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "list_trade_latencies",
                    source,
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "list_trade_latencies",
                    source,
                })?;

            rows.into_iter()
                .map(|row| {
                    let (action_id, strategy_id, recorded_at, timestamps_json, latency_json) = row
                        .map_err(|source| PersistenceError::Sqlite {
                            operation: "list_trade_latencies",
                            source,
                        })?;
                    Ok(TradePathLatencyRecord {
                        action_id,
                        strategy_id,
                        recorded_at: timestamp_from_storage("recorded_at", &recorded_at)?,
                        timestamps: serde_json::from_str(&timestamps_json)
                            .map_err(|source| PersistenceError::Serialization { source })?,
                        latency: serde_json::from_str(&latency_json)
                            .map_err(|source| PersistenceError::Serialization { source })?,
                    })
                })
                .collect()
        })
    }
}

impl SystemHealthStore for SqlitePersistence {
    fn append_system_health(&self, snapshot: SystemHealthSnapshot) -> Result<(), PersistenceError> {
        self.with_connection("append_system_health", |connection| {
            let snapshot_json = serde_json::to_string(&snapshot)
                .map_err(|source| PersistenceError::Serialization { source })?;
            connection
                .execute(
                    r#"
                    INSERT INTO system_health (
                        updated_at,
                        cpu_percent,
                        memory_bytes,
                        reconnect_count,
                        db_write_latency_ms,
                        queue_lag_ms,
                        error_count,
                        feed_degraded,
                        snapshot_json
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    "#,
                    params![
                        timestamp_to_storage(&snapshot.updated_at),
                        snapshot.cpu_percent,
                        snapshot.memory_bytes.map(|value| value as i64),
                        snapshot.reconnect_count as i64,
                        snapshot.db_write_latency_ms.map(|value| value as i64),
                        snapshot.queue_lag_ms.map(|value| value as i64),
                        snapshot.error_count as i64,
                        snapshot.feed_degraded,
                        snapshot_json,
                    ],
                )
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "append_system_health",
                    source,
                })?;
            Ok(())
        })
    }

    fn list_system_health(&self) -> Result<Vec<SystemHealthSnapshot>, PersistenceError> {
        self.with_connection("list_system_health", |connection| {
            let mut statement = connection
                .prepare(
                    r#"
                    SELECT snapshot_json
                    FROM system_health
                    ORDER BY updated_at ASC, id ASC
                    "#,
                )
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "list_system_health",
                    source,
                })?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|source| PersistenceError::Sqlite {
                    operation: "list_system_health",
                    source,
                })?;

            rows.into_iter()
                .map(|row| {
                    let snapshot_json = row.map_err(|source| PersistenceError::Sqlite {
                        operation: "list_system_health",
                        source,
                    })?;
                    serde_json::from_str(&snapshot_json)
                        .map_err(|source| PersistenceError::Serialization { source })
                })
                .collect()
        })
    }
}

impl StrategyRunStore for SqlitePersistence {
    fn append_strategy_run(&self, record: StrategyRunRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_strategy_run", |connection| {
            sqlite_upsert_json_record(
                connection,
                "append_strategy_run",
                "strategy_runs",
                &record.run_id,
                Some(&record.strategy_id),
                Some(&record.run_id),
                None,
                &record.started_at,
                &record,
            )
        })
    }

    fn list_strategy_runs(&self) -> Result<Vec<StrategyRunRecord>, PersistenceError> {
        self.with_connection("list_strategy_runs", |connection| {
            sqlite_list_json_records(connection, "list_strategy_runs", "strategy_runs")
        })
    }
}

impl OrderStore for SqlitePersistence {
    fn append_order(&self, record: OrderRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_order", |connection| {
            sqlite_upsert_json_record(
                connection,
                "append_order",
                "order_records",
                &record.broker_order_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.updated_at,
                &record,
            )
        })
    }

    fn list_orders(&self) -> Result<Vec<OrderRecord>, PersistenceError> {
        self.with_connection("list_orders", |connection| {
            sqlite_list_json_records(connection, "list_orders", "order_records")
        })
    }
}

impl FillStore for SqlitePersistence {
    fn append_fill(&self, record: FillRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_fill", |connection| {
            sqlite_upsert_json_record(
                connection,
                "append_fill",
                "fill_records",
                &record.fill_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.occurred_at,
                &record,
            )
        })
    }

    fn list_fills(&self) -> Result<Vec<FillRecord>, PersistenceError> {
        self.with_connection("list_fills", |connection| {
            sqlite_list_json_records(connection, "list_fills", "fill_records")
        })
    }
}

impl PositionStore for SqlitePersistence {
    fn append_position(&self, record: PositionRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_position", |connection| {
            sqlite_upsert_json_record(
                connection,
                "append_position",
                "position_records",
                &record.record_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.captured_at,
                &record,
            )
        })
    }

    fn list_positions(&self) -> Result<Vec<PositionRecord>, PersistenceError> {
        self.with_connection("list_positions", |connection| {
            sqlite_list_json_records(connection, "list_positions", "position_records")
        })
    }
}

impl PnlSnapshotStore for SqlitePersistence {
    fn append_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_pnl_snapshot", |connection| {
            sqlite_upsert_json_record(
                connection,
                "append_pnl_snapshot",
                "pnl_snapshot_records",
                &record.snapshot_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                record.symbol.as_deref(),
                &record.captured_at,
                &record,
            )
        })
    }

    fn list_pnl_snapshots(&self) -> Result<Vec<PnlSnapshotRecord>, PersistenceError> {
        self.with_connection("list_pnl_snapshots", |connection| {
            sqlite_list_json_records(connection, "list_pnl_snapshots", "pnl_snapshot_records")
        })
    }
}

impl TradeSummaryStore for SqlitePersistence {
    fn append_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), PersistenceError> {
        self.with_connection("append_trade_summary", |connection| {
            sqlite_upsert_json_record(
                connection,
                "append_trade_summary",
                "trade_summary_records",
                &record.trade_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.closed_at.unwrap_or(record.opened_at),
                &record,
            )
        })
    }

    fn list_trade_summaries(&self) -> Result<Vec<TradeSummaryRecord>, PersistenceError> {
        self.with_connection("list_trade_summaries", |connection| {
            sqlite_list_json_records(connection, "list_trade_summaries", "trade_summary_records")
        })
    }
}

#[derive(Clone, Debug)]
pub struct PostgresPersistence {
    connection_string: String,
}

impl PostgresPersistence {
    pub fn connect(connection_string: String) -> Result<Self, PersistenceError> {
        let store = Self { connection_string };
        store.with_client("connect", |client| {
            init_postgres_schema(client)?;
            Ok(())
        })?;
        Ok(store)
    }

    fn with_client<T>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&mut PostgresClient) -> Result<T, PersistenceError>,
    ) -> Result<T, PersistenceError> {
        let mut client = PostgresClient::connect(&self.connection_string, NoTls)
            .map_err(|source| PersistenceError::Postgres { operation, source })?;
        init_postgres_schema(&mut client)?;
        f(&mut client)
    }
}

impl EventJournalStore for PostgresPersistence {
    fn append_event(&self, record: EventJournalRecord) -> Result<(), PersistenceError> {
        self.with_client("append_event", |client| {
            let payload_json = serde_json::to_string(&record.payload)
                .map_err(|source| PersistenceError::Serialization { source })?;
            client
                .execute(
                    r#"
                    INSERT INTO event_journal (
                        event_id,
                        category,
                        action,
                        source,
                        severity,
                        occurred_at,
                        payload_json
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    ON CONFLICT (event_id) DO UPDATE SET
                        category = EXCLUDED.category,
                        action = EXCLUDED.action,
                        source = EXCLUDED.source,
                        severity = EXCLUDED.severity,
                        occurred_at = EXCLUDED.occurred_at,
                        payload_json = EXCLUDED.payload_json
                    "#,
                    &[
                        &record.event_id,
                        &record.category,
                        &record.action,
                        &enum_to_storage(&record.source)?,
                        &enum_to_storage(&record.severity)?,
                        &timestamp_to_storage(&record.occurred_at),
                        &payload_json,
                    ],
                )
                .map_err(|source| PersistenceError::Postgres {
                    operation: "append_event",
                    source,
                })?;
            Ok(())
        })
    }

    fn list_events(&self) -> Result<Vec<EventJournalRecord>, PersistenceError> {
        self.with_client("list_events", |client| {
            client
                .query(
                    r#"
                    SELECT event_id, category, action, source, severity, occurred_at, payload_json
                    FROM event_journal
                    ORDER BY occurred_at ASC, event_id ASC
                    "#,
                    &[],
                )
                .map_err(|source| PersistenceError::Postgres {
                    operation: "list_events",
                    source,
                })?
                .into_iter()
                .map(|row| {
                    let source: String = row.get(3);
                    let severity: String = row.get(4);
                    let occurred_at: String = row.get(5);
                    let payload_json: String = row.get(6);
                    Ok(EventJournalRecord {
                        event_id: row.get(0),
                        category: row.get(1),
                        action: row.get(2),
                        source: enum_from_storage(&source)?,
                        severity: enum_from_storage(&severity)?,
                        occurred_at: timestamp_from_storage("occurred_at", &occurred_at)?,
                        payload: serde_json::from_str(&payload_json)
                            .map_err(|source| PersistenceError::Serialization { source })?,
                    })
                })
                .collect()
        })
    }
}

impl TradeLatencyStore for PostgresPersistence {
    fn append_trade_latency(&self, record: TradePathLatencyRecord) -> Result<(), PersistenceError> {
        self.with_client("append_trade_latency", |client| {
            let timestamps_json = serde_json::to_string(&record.timestamps)
                .map_err(|source| PersistenceError::Serialization { source })?;
            let latency_json = serde_json::to_string(&record.latency)
                .map_err(|source| PersistenceError::Serialization { source })?;
            client
                .execute(
                    r#"
                    INSERT INTO trade_path_latency (
                        action_id,
                        strategy_id,
                        recorded_at,
                        timestamps_json,
                        latency_json
                    ) VALUES ($1, $2, $3, $4, $5)
                    ON CONFLICT (action_id) DO UPDATE SET
                        strategy_id = EXCLUDED.strategy_id,
                        recorded_at = EXCLUDED.recorded_at,
                        timestamps_json = EXCLUDED.timestamps_json,
                        latency_json = EXCLUDED.latency_json
                    "#,
                    &[
                        &record.action_id,
                        &record.strategy_id,
                        &timestamp_to_storage(&record.recorded_at),
                        &timestamps_json,
                        &latency_json,
                    ],
                )
                .map_err(|source| PersistenceError::Postgres {
                    operation: "append_trade_latency",
                    source,
                })?;
            Ok(())
        })
    }

    fn list_trade_latencies(&self) -> Result<Vec<TradePathLatencyRecord>, PersistenceError> {
        self.with_client("list_trade_latencies", |client| {
            client
                .query(
                    r#"
                    SELECT action_id, strategy_id, recorded_at, timestamps_json, latency_json
                    FROM trade_path_latency
                    ORDER BY recorded_at ASC, action_id ASC
                    "#,
                    &[],
                )
                .map_err(|source| PersistenceError::Postgres {
                    operation: "list_trade_latencies",
                    source,
                })?
                .into_iter()
                .map(|row| {
                    let recorded_at: String = row.get(2);
                    let timestamps_json: String = row.get(3);
                    let latency_json: String = row.get(4);
                    Ok(TradePathLatencyRecord {
                        action_id: row.get(0),
                        strategy_id: row.get(1),
                        recorded_at: timestamp_from_storage("recorded_at", &recorded_at)?,
                        timestamps: serde_json::from_str(&timestamps_json)
                            .map_err(|source| PersistenceError::Serialization { source })?,
                        latency: serde_json::from_str(&latency_json)
                            .map_err(|source| PersistenceError::Serialization { source })?,
                    })
                })
                .collect()
        })
    }
}

impl SystemHealthStore for PostgresPersistence {
    fn append_system_health(&self, snapshot: SystemHealthSnapshot) -> Result<(), PersistenceError> {
        self.with_client("append_system_health", |client| {
            let snapshot_json = serde_json::to_string(&snapshot)
                .map_err(|source| PersistenceError::Serialization { source })?;
            client
                .execute(
                    r#"
                    INSERT INTO system_health (
                        updated_at,
                        cpu_percent,
                        memory_bytes,
                        reconnect_count,
                        db_write_latency_ms,
                        queue_lag_ms,
                        error_count,
                        feed_degraded,
                        snapshot_json
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    "#,
                    &[
                        &timestamp_to_storage(&snapshot.updated_at),
                        &snapshot.cpu_percent,
                        &(snapshot.memory_bytes.map(|value| value as i64)),
                        &(snapshot.reconnect_count as i64),
                        &(snapshot.db_write_latency_ms.map(|value| value as i64)),
                        &(snapshot.queue_lag_ms.map(|value| value as i64)),
                        &(snapshot.error_count as i64),
                        &snapshot.feed_degraded,
                        &snapshot_json,
                    ],
                )
                .map_err(|source| PersistenceError::Postgres {
                    operation: "append_system_health",
                    source,
                })?;
            Ok(())
        })
    }

    fn list_system_health(&self) -> Result<Vec<SystemHealthSnapshot>, PersistenceError> {
        self.with_client("list_system_health", |client| {
            client
                .query(
                    r#"
                    SELECT snapshot_json
                    FROM system_health
                    ORDER BY updated_at ASC, id ASC
                    "#,
                    &[],
                )
                .map_err(|source| PersistenceError::Postgres {
                    operation: "list_system_health",
                    source,
                })?
                .into_iter()
                .map(|row| {
                    let snapshot_json: String = row.get(0);
                    serde_json::from_str(&snapshot_json)
                        .map_err(|source| PersistenceError::Serialization { source })
                })
                .collect()
        })
    }
}

impl StrategyRunStore for PostgresPersistence {
    fn append_strategy_run(&self, record: StrategyRunRecord) -> Result<(), PersistenceError> {
        self.with_client("append_strategy_run", |client| {
            postgres_upsert_json_record(
                client,
                "append_strategy_run",
                "strategy_runs",
                &record.run_id,
                Some(&record.strategy_id),
                Some(&record.run_id),
                None,
                &record.ended_at.unwrap_or(record.started_at),
                &record,
            )
        })
    }

    fn list_strategy_runs(&self) -> Result<Vec<StrategyRunRecord>, PersistenceError> {
        self.with_client("list_strategy_runs", |client| {
            postgres_list_json_records(client, "list_strategy_runs", "strategy_runs")
        })
    }
}

impl OrderStore for PostgresPersistence {
    fn append_order(&self, record: OrderRecord) -> Result<(), PersistenceError> {
        self.with_client("append_order", |client| {
            postgres_upsert_json_record(
                client,
                "append_order",
                "order_records",
                &record.broker_order_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.updated_at,
                &record,
            )
        })
    }

    fn list_orders(&self) -> Result<Vec<OrderRecord>, PersistenceError> {
        self.with_client("list_orders", |client| {
            postgres_list_json_records(client, "list_orders", "order_records")
        })
    }
}

impl FillStore for PostgresPersistence {
    fn append_fill(&self, record: FillRecord) -> Result<(), PersistenceError> {
        self.with_client("append_fill", |client| {
            postgres_upsert_json_record(
                client,
                "append_fill",
                "fill_records",
                &record.fill_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.occurred_at,
                &record,
            )
        })
    }

    fn list_fills(&self) -> Result<Vec<FillRecord>, PersistenceError> {
        self.with_client("list_fills", |client| {
            postgres_list_json_records(client, "list_fills", "fill_records")
        })
    }
}

impl PositionStore for PostgresPersistence {
    fn append_position(&self, record: PositionRecord) -> Result<(), PersistenceError> {
        self.with_client("append_position", |client| {
            postgres_upsert_json_record(
                client,
                "append_position",
                "position_records",
                &record.record_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.captured_at,
                &record,
            )
        })
    }

    fn list_positions(&self) -> Result<Vec<PositionRecord>, PersistenceError> {
        self.with_client("list_positions", |client| {
            postgres_list_json_records(client, "list_positions", "position_records")
        })
    }
}

impl PnlSnapshotStore for PostgresPersistence {
    fn append_pnl_snapshot(&self, record: PnlSnapshotRecord) -> Result<(), PersistenceError> {
        self.with_client("append_pnl_snapshot", |client| {
            postgres_upsert_json_record(
                client,
                "append_pnl_snapshot",
                "pnl_snapshot_records",
                &record.snapshot_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                record.symbol.as_deref(),
                &record.captured_at,
                &record,
            )
        })
    }

    fn list_pnl_snapshots(&self) -> Result<Vec<PnlSnapshotRecord>, PersistenceError> {
        self.with_client("list_pnl_snapshots", |client| {
            postgres_list_json_records(client, "list_pnl_snapshots", "pnl_snapshot_records")
        })
    }
}

impl TradeSummaryStore for PostgresPersistence {
    fn append_trade_summary(&self, record: TradeSummaryRecord) -> Result<(), PersistenceError> {
        self.with_client("append_trade_summary", |client| {
            postgres_upsert_json_record(
                client,
                "append_trade_summary",
                "trade_summary_records",
                &record.trade_id,
                record.strategy_id.as_deref(),
                record.run_id.as_deref(),
                Some(&record.symbol),
                &record.closed_at.unwrap_or(record.opened_at),
                &record,
            )
        })
    }

    fn list_trade_summaries(&self) -> Result<Vec<TradeSummaryRecord>, PersistenceError> {
        self.with_client("list_trade_summaries", |client| {
            postgres_list_json_records(client, "list_trade_summaries", "trade_summary_records")
        })
    }
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("persistence storage lock is poisoned")]
    Poisoned,
    #[error("failed to prepare filesystem path `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sqlite persistence {operation} failed: {source}")]
    Sqlite {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("postgres persistence {operation} failed: {source}")]
    Postgres {
        operation: &'static str,
        #[source]
        source: postgres::Error,
    },
    #[error("failed to serialize persistence payload: {source}")]
    Serialization {
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to parse stored timestamp `{field}` value `{value}`: {source}")]
    TimestampParse {
        field: &'static str,
        value: String,
        #[source]
        source: chrono::ParseError,
    },
}

fn ensure_parent_dir(path: &Path) -> Result<(), PersistenceError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn init_sqlite_schema(connection: &SqliteConnection) -> Result<(), PersistenceError> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS event_journal (
                event_id TEXT PRIMARY KEY,
                category TEXT NOT NULL,
                action TEXT NOT NULL,
                source TEXT NOT NULL,
                severity TEXT NOT NULL,
                occurred_at TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS event_journal_occurred_idx
                ON event_journal (occurred_at);

            CREATE TABLE IF NOT EXISTS trade_path_latency (
                action_id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                recorded_at TEXT NOT NULL,
                timestamps_json TEXT NOT NULL,
                latency_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trade_path_latency_recorded_idx
                ON trade_path_latency (recorded_at);

            CREATE TABLE IF NOT EXISTS system_health (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                updated_at TEXT NOT NULL,
                cpu_percent REAL NULL,
                memory_bytes INTEGER NULL,
                reconnect_count INTEGER NOT NULL,
                db_write_latency_ms INTEGER NULL,
                queue_lag_ms INTEGER NULL,
                error_count INTEGER NOT NULL,
                feed_degraded INTEGER NOT NULL,
                snapshot_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS system_health_updated_idx
                ON system_health (updated_at);

            CREATE TABLE IF NOT EXISTS strategy_runs (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS strategy_runs_happened_idx
                ON strategy_runs (happened_at);

            CREATE TABLE IF NOT EXISTS order_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS order_records_happened_idx
                ON order_records (happened_at);

            CREATE TABLE IF NOT EXISTS fill_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS fill_records_happened_idx
                ON fill_records (happened_at);

            CREATE TABLE IF NOT EXISTS position_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS position_records_happened_idx
                ON position_records (happened_at);

            CREATE TABLE IF NOT EXISTS pnl_snapshot_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS pnl_snapshot_records_happened_idx
                ON pnl_snapshot_records (happened_at);

            CREATE TABLE IF NOT EXISTS trade_summary_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trade_summary_records_happened_idx
                ON trade_summary_records (happened_at);
            "#,
        )
        .map_err(|source| PersistenceError::Sqlite {
            operation: "init_schema",
            source,
        })
}

fn init_postgres_schema(client: &mut PostgresClient) -> Result<(), PersistenceError> {
    client
        .batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS event_journal (
                event_id TEXT PRIMARY KEY,
                category TEXT NOT NULL,
                action TEXT NOT NULL,
                source TEXT NOT NULL,
                severity TEXT NOT NULL,
                occurred_at TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS event_journal_occurred_idx
                ON event_journal (occurred_at);

            CREATE TABLE IF NOT EXISTS trade_path_latency (
                action_id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                recorded_at TEXT NOT NULL,
                timestamps_json TEXT NOT NULL,
                latency_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trade_path_latency_recorded_idx
                ON trade_path_latency (recorded_at);

            CREATE TABLE IF NOT EXISTS system_health (
                id BIGSERIAL PRIMARY KEY,
                updated_at TEXT NOT NULL,
                cpu_percent DOUBLE PRECISION NULL,
                memory_bytes BIGINT NULL,
                reconnect_count BIGINT NOT NULL,
                db_write_latency_ms BIGINT NULL,
                queue_lag_ms BIGINT NULL,
                error_count BIGINT NOT NULL,
                feed_degraded BOOLEAN NOT NULL,
                snapshot_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS system_health_updated_idx
                ON system_health (updated_at);

            CREATE TABLE IF NOT EXISTS strategy_runs (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS strategy_runs_happened_idx
                ON strategy_runs (happened_at);

            CREATE TABLE IF NOT EXISTS order_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS order_records_happened_idx
                ON order_records (happened_at);

            CREATE TABLE IF NOT EXISTS fill_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS fill_records_happened_idx
                ON fill_records (happened_at);

            CREATE TABLE IF NOT EXISTS position_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS position_records_happened_idx
                ON position_records (happened_at);

            CREATE TABLE IF NOT EXISTS pnl_snapshot_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS pnl_snapshot_records_happened_idx
                ON pnl_snapshot_records (happened_at);

            CREATE TABLE IF NOT EXISTS trade_summary_records (
                id TEXT PRIMARY KEY,
                strategy_id TEXT NULL,
                run_id TEXT NULL,
                symbol TEXT NULL,
                happened_at TEXT NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trade_summary_records_happened_idx
                ON trade_summary_records (happened_at);
            "#,
        )
        .map_err(|source| PersistenceError::Postgres {
            operation: "init_schema",
            source,
        })
}

fn timestamp_to_storage(value: &DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn timestamp_from_storage(
    field: &'static str,
    raw: &str,
) -> Result<DateTime<Utc>, PersistenceError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|source| PersistenceError::TimestampParse {
            field,
            value: raw.to_owned(),
            source,
        })
}

fn enum_to_storage<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    match serde_json::to_value(value)
        .map_err(|source| PersistenceError::Serialization { source })?
    {
        Value::String(text) => Ok(text),
        other => Ok(other.to_string()),
    }
}

fn enum_from_storage<T: DeserializeOwned>(value: &str) -> Result<T, PersistenceError> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|source| PersistenceError::Serialization { source })
}

fn sqlite_upsert_json_record<T: Serialize>(
    connection: &SqliteConnection,
    operation: &'static str,
    table: &str,
    id: &str,
    strategy_id: Option<&str>,
    run_id: Option<&str>,
    symbol: Option<&str>,
    happened_at: &DateTime<Utc>,
    record: &T,
) -> Result<(), PersistenceError> {
    let record_json = serde_json::to_string(record)
        .map_err(|source| PersistenceError::Serialization { source })?;
    let sql = format!(
        "INSERT OR REPLACE INTO {table} (id, strategy_id, run_id, symbol, happened_at, record_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
    );
    connection
        .execute(
            &sql,
            params![
                id,
                strategy_id,
                run_id,
                symbol,
                timestamp_to_storage(happened_at),
                record_json,
            ],
        )
        .map_err(|source| PersistenceError::Sqlite { operation, source })?;
    Ok(())
}

fn sqlite_list_json_records<T: DeserializeOwned>(
    connection: &SqliteConnection,
    operation: &'static str,
    table: &str,
) -> Result<Vec<T>, PersistenceError> {
    let sql = format!("SELECT record_json FROM {table} ORDER BY happened_at ASC, id ASC");
    let mut statement = connection
        .prepare(&sql)
        .map_err(|source| PersistenceError::Sqlite { operation, source })?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| PersistenceError::Sqlite { operation, source })?;

    rows.into_iter()
        .map(|row| {
            let record_json =
                row.map_err(|source| PersistenceError::Sqlite { operation, source })?;
            serde_json::from_str(&record_json)
                .map_err(|source| PersistenceError::Serialization { source })
        })
        .collect()
}

fn postgres_upsert_json_record<T: Serialize>(
    client: &mut PostgresClient,
    operation: &'static str,
    table: &str,
    id: &str,
    strategy_id: Option<&str>,
    run_id: Option<&str>,
    symbol: Option<&str>,
    happened_at: &DateTime<Utc>,
    record: &T,
) -> Result<(), PersistenceError> {
    let record_json = serde_json::to_string(record)
        .map_err(|source| PersistenceError::Serialization { source })?;
    let sql = format!(
        "INSERT INTO {table} (id, strategy_id, run_id, symbol, happened_at, record_json) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (id) DO UPDATE SET strategy_id = EXCLUDED.strategy_id, run_id = EXCLUDED.run_id, symbol = EXCLUDED.symbol, happened_at = EXCLUDED.happened_at, record_json = EXCLUDED.record_json"
    );
    client
        .execute(
            &sql,
            &[
                &id,
                &strategy_id,
                &run_id,
                &symbol,
                &timestamp_to_storage(happened_at),
                &record_json,
            ],
        )
        .map_err(|source| PersistenceError::Postgres { operation, source })?;
    Ok(())
}

fn postgres_list_json_records<T: DeserializeOwned>(
    client: &mut PostgresClient,
    operation: &'static str,
    table: &str,
) -> Result<Vec<T>, PersistenceError> {
    let sql = format!("SELECT record_json FROM {table} ORDER BY happened_at ASC, id ASC");
    client
        .query(&sql, &[])
        .map_err(|source| PersistenceError::Postgres { operation, source })?
        .into_iter()
        .map(|row| {
            let record_json: String = row.get(0);
            serde_json::from_str(&record_json)
                .map_err(|source| PersistenceError::Serialization { source })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        time::{SystemTime, UNIX_EPOCH},
    };

    use chrono::{Duration, Utc};
    use rust_decimal::Decimal;
    use serde_json::json;
    use tv_bot_config::{AppConfig, ConfigError, MapEnvironment};
    use tv_bot_core_types::{
        ActionSource, BrokerOrderStatus, EntryOrderType, EventSeverity, StrategyRunStatus,
        TradePathLatencySnapshot, TradePathTimestamps, TradeSide, TradeSummaryStatus,
    };

    use super::*;

    fn load_test_config(values: &[(&str, &str)]) -> Result<AppConfig, ConfigError> {
        let env = values
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>();

        AppConfig::load(None, &MapEnvironment::new(env))
    }

    fn unique_temp_path(stem: &str, ext: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("{stem}_{unique}.{ext}"))
    }

    fn sample_event(occurred_at: DateTime<Utc>) -> EventJournalRecord {
        EventJournalRecord {
            event_id: "evt-1".to_owned(),
            category: "runtime".to_owned(),
            action: "armed".to_owned(),
            source: ActionSource::Cli,
            severity: EventSeverity::Info,
            occurred_at,
            payload: json!({ "mode": "paper" }),
        }
    }

    fn sample_latency(occurred_at: DateTime<Utc>) -> TradePathLatencyRecord {
        TradePathLatencyRecord {
            action_id: "act-1".to_owned(),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            recorded_at: occurred_at,
            timestamps: TradePathTimestamps {
                market_event_at: Some(occurred_at),
                signal_at: Some(occurred_at + Duration::milliseconds(5)),
                decision_at: Some(occurred_at + Duration::milliseconds(9)),
                order_sent_at: Some(occurred_at + Duration::milliseconds(12)),
                broker_ack_at: Some(occurred_at + Duration::milliseconds(18)),
                fill_at: Some(occurred_at + Duration::milliseconds(31)),
                sync_update_at: Some(occurred_at + Duration::milliseconds(44)),
            },
            latency: TradePathLatencySnapshot {
                signal_latency_ms: Some(5),
                decision_latency_ms: Some(4),
                order_send_latency_ms: Some(3),
                broker_ack_latency_ms: Some(6),
                fill_latency_ms: Some(13),
                sync_update_latency_ms: Some(13),
                end_to_end_fill_latency_ms: Some(31),
                end_to_end_sync_latency_ms: Some(44),
            },
        }
    }

    fn sample_health(occurred_at: DateTime<Utc>) -> SystemHealthSnapshot {
        SystemHealthSnapshot {
            cpu_percent: Some(12.5),
            memory_bytes: Some(1_024),
            reconnect_count: 1,
            db_write_latency_ms: Some(4),
            queue_lag_ms: Some(0),
            error_count: 0,
            feed_degraded: false,
            updated_at: occurred_at,
        }
    }

    fn sample_strategy_run(occurred_at: DateTime<Utc>) -> StrategyRunRecord {
        StrategyRunRecord {
            run_id: "run-1".to_owned(),
            strategy_id: "gc_phase_6_v1".to_owned(),
            mode: tv_bot_core_types::RuntimeMode::Paper,
            status: StrategyRunStatus::Active,
            trigger_source: ActionSource::System,
            started_at: occurred_at,
            ended_at: None,
            note: Some("phase 6 durability test".to_owned()),
        }
    }

    fn sample_order(occurred_at: DateTime<Utc>) -> OrderRecord {
        OrderRecord {
            broker_order_id: "ord-1".to_owned(),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            run_id: Some("run-1".to_owned()),
            account_id: Some("acct-paper".to_owned()),
            symbol: "GCM6".to_owned(),
            side: TradeSide::Buy,
            order_type: Some(EntryOrderType::Limit),
            quantity: 2,
            filled_quantity: 1,
            average_fill_price: Some(Decimal::new(334550, 2)),
            status: BrokerOrderStatus::Working,
            provider: "tradovate".to_owned(),
            submitted_at: occurred_at,
            updated_at: occurred_at + Duration::milliseconds(25),
        }
    }

    fn sample_fill(occurred_at: DateTime<Utc>) -> FillRecord {
        FillRecord {
            fill_id: "fill-1".to_owned(),
            broker_order_id: Some("ord-1".to_owned()),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            run_id: Some("run-1".to_owned()),
            account_id: Some("acct-paper".to_owned()),
            symbol: "GCM6".to_owned(),
            side: TradeSide::Buy,
            quantity: 1,
            price: Decimal::new(334550, 2),
            fee: Decimal::new(125, 2),
            commission: Decimal::new(75, 2),
            occurred_at: occurred_at + Duration::milliseconds(40),
        }
    }

    fn sample_position(occurred_at: DateTime<Utc>) -> PositionRecord {
        PositionRecord {
            record_id: "pos-1".to_owned(),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            run_id: Some("run-1".to_owned()),
            account_id: Some("acct-paper".to_owned()),
            symbol: "GCM6".to_owned(),
            quantity: 1,
            average_price: Some(Decimal::new(334550, 2)),
            realized_pnl: Some(Decimal::new(0, 0)),
            unrealized_pnl: Some(Decimal::new(850, 2)),
            protective_orders_present: true,
            captured_at: occurred_at + Duration::milliseconds(50),
        }
    }

    fn sample_pnl_snapshot(occurred_at: DateTime<Utc>) -> PnlSnapshotRecord {
        PnlSnapshotRecord {
            snapshot_id: "pnl-1".to_owned(),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            run_id: Some("run-1".to_owned()),
            account_id: Some("acct-paper".to_owned()),
            symbol: Some("GCM6".to_owned()),
            gross_pnl: Decimal::new(1200, 2),
            net_pnl: Decimal::new(1000, 2),
            fees: Decimal::new(125, 2),
            commissions: Decimal::new(75, 2),
            slippage: Decimal::new(0, 0),
            realized_pnl: Some(Decimal::new(0, 0)),
            unrealized_pnl: Some(Decimal::new(1000, 2)),
            captured_at: occurred_at + Duration::milliseconds(60),
        }
    }

    fn sample_trade_summary(occurred_at: DateTime<Utc>) -> TradeSummaryRecord {
        TradeSummaryRecord {
            trade_id: "trade-1".to_owned(),
            strategy_id: Some("gc_phase_6_v1".to_owned()),
            run_id: Some("run-1".to_owned()),
            account_id: Some("acct-paper".to_owned()),
            symbol: "GCM6".to_owned(),
            side: TradeSide::Buy,
            status: TradeSummaryStatus::Closed,
            quantity: 1,
            average_entry_price: Decimal::new(334550, 2),
            average_exit_price: Some(Decimal::new(335150, 2)),
            opened_at: occurred_at,
            closed_at: Some(occurred_at + Duration::seconds(30)),
            gross_pnl: Decimal::new(600, 2),
            net_pnl: Decimal::new(400, 2),
            fees: Decimal::new(125, 2),
            commissions: Decimal::new(75, 2),
            slippage: Decimal::new(0, 0),
        }
    }

    #[test]
    fn persistence_plan_prefers_primary_when_configured() {
        let config = load_test_config(&[
            ("TV_BOT__RUNTIME__STARTUP_MODE", "paper"),
            (
                "TV_BOT__PERSISTENCE__PRIMARY_URL",
                "postgres://postgres@localhost/tv_bot",
            ),
            ("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED", "true"),
            (
                "TV_BOT__PERSISTENCE__SQLITE_FALLBACK_PATH",
                "data/fallback.sqlite",
            ),
        ])
        .expect("config should load");

        let plan = PersistencePlan::from_config(&config);
        assert_eq!(plan.mode, PersistenceStorageMode::PrimaryConfigured);
        assert!(plan.primary_configured);
        assert!(plan.sqlite_fallback_enabled);
        assert_eq!(plan.sqlite_path, PathBuf::from("data/fallback.sqlite"));
    }

    #[test]
    fn persistence_plan_marks_sqlite_only_without_primary() {
        let config = load_test_config(&[
            ("TV_BOT__RUNTIME__STARTUP_MODE", "paper"),
            ("TV_BOT__RUNTIME__ALLOW_SQLITE_FALLBACK", "true"),
            ("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED", "true"),
        ])
        .expect("config should load");

        let plan = PersistencePlan::from_config(&config);
        assert_eq!(plan.mode, PersistenceStorageMode::SqliteFallbackOnly);
        assert!(plan.allow_runtime_fallback);
        assert!(plan
            .detail
            .contains("SQLite fallback is configured without a primary Postgres backend"));
    }

    #[test]
    fn persistence_plan_marks_unconfigured_when_no_backend_exists() {
        let config = load_test_config(&[("TV_BOT__RUNTIME__STARTUP_MODE", "paper")])
            .expect("config should load");

        let plan = PersistencePlan::from_config(&config);
        assert_eq!(plan.mode, PersistenceStorageMode::Unconfigured);
        assert!(!plan.primary_configured);
        assert!(!plan.sqlite_fallback_enabled);
    }

    #[test]
    fn in_memory_persistence_records_events_latency_and_health() {
        let persistence = InMemoryPersistence::new();
        let occurred_at = Utc::now();
        let event = sample_event(occurred_at);
        let latency = sample_latency(occurred_at);
        let health = sample_health(occurred_at);

        persistence
            .append_event(event.clone())
            .expect("event should append");
        persistence
            .append_trade_latency(latency.clone())
            .expect("latency should append");
        persistence
            .append_system_health(health.clone())
            .expect("health should append");

        assert_eq!(
            persistence.list_events().expect("events should list"),
            vec![event]
        );
        assert_eq!(
            persistence
                .list_trade_latencies()
                .expect("latencies should list"),
            vec![latency]
        );
        assert_eq!(
            persistence
                .list_system_health()
                .expect("health should list"),
            vec![health]
        );
    }

    #[test]
    fn in_memory_persistence_records_trading_history() {
        let persistence = InMemoryPersistence::new();
        let occurred_at = Utc::now();
        let run = sample_strategy_run(occurred_at);
        let order = sample_order(occurred_at);
        let fill = sample_fill(occurred_at);
        let position = sample_position(occurred_at);
        let pnl = sample_pnl_snapshot(occurred_at);
        let trade = sample_trade_summary(occurred_at);

        persistence
            .append_strategy_run(run.clone())
            .expect("run should append");
        persistence
            .append_order(order.clone())
            .expect("order should append");
        persistence
            .append_fill(fill.clone())
            .expect("fill should append");
        persistence
            .append_position(position.clone())
            .expect("position should append");
        persistence
            .append_pnl_snapshot(pnl.clone())
            .expect("pnl should append");
        persistence
            .append_trade_summary(trade.clone())
            .expect("trade summary should append");

        assert_eq!(
            persistence.list_strategy_runs().expect("runs should list"),
            vec![run]
        );
        assert_eq!(
            persistence.list_orders().expect("orders should list"),
            vec![order]
        );
        assert_eq!(
            persistence.list_fills().expect("fills should list"),
            vec![fill]
        );
        assert_eq!(
            persistence.list_positions().expect("positions should list"),
            vec![position]
        );
        assert_eq!(
            persistence
                .list_pnl_snapshots()
                .expect("pnl snapshots should list"),
            vec![pnl]
        );
        assert_eq!(
            persistence
                .list_trade_summaries()
                .expect("trade summaries should list"),
            vec![trade]
        );
    }

    #[test]
    fn sqlite_persistence_records_events_latency_and_health() {
        let sqlite_path = unique_temp_path("tv_bot_persistence_sqlite", "db");
        let persistence = SqlitePersistence::open(sqlite_path.clone()).expect("sqlite should open");
        let occurred_at = Utc::now();

        persistence
            .append_event(sample_event(occurred_at))
            .expect("event should persist");
        persistence
            .append_trade_latency(sample_latency(occurred_at))
            .expect("latency should persist");
        persistence
            .append_system_health(sample_health(occurred_at))
            .expect("health should persist");

        assert_eq!(
            persistence.list_events().expect("events should list").len(),
            1
        );
        assert_eq!(
            persistence
                .list_trade_latencies()
                .expect("latencies should list")
                .len(),
            1
        );
        assert_eq!(
            persistence
                .list_system_health()
                .expect("health should list")
                .len(),
            1
        );

        let _ = fs::remove_file(sqlite_path);
    }

    #[test]
    fn sqlite_persistence_records_trading_history() {
        let sqlite_path = unique_temp_path("tv_bot_persistence_sqlite_history", "db");
        let persistence = SqlitePersistence::open(sqlite_path.clone()).expect("sqlite should open");
        let occurred_at = Utc::now();

        persistence
            .append_strategy_run(sample_strategy_run(occurred_at))
            .expect("run should persist");
        persistence
            .append_order(sample_order(occurred_at))
            .expect("order should persist");
        persistence
            .append_fill(sample_fill(occurred_at))
            .expect("fill should persist");
        persistence
            .append_position(sample_position(occurred_at))
            .expect("position should persist");
        persistence
            .append_pnl_snapshot(sample_pnl_snapshot(occurred_at))
            .expect("pnl should persist");
        persistence
            .append_trade_summary(sample_trade_summary(occurred_at))
            .expect("trade summary should persist");

        assert_eq!(
            persistence
                .list_strategy_runs()
                .expect("runs should list")
                .len(),
            1
        );
        assert_eq!(
            persistence.list_orders().expect("orders should list").len(),
            1
        );
        assert_eq!(
            persistence.list_fills().expect("fills should list").len(),
            1
        );
        assert_eq!(
            persistence
                .list_positions()
                .expect("positions should list")
                .len(),
            1
        );
        assert_eq!(
            persistence
                .list_pnl_snapshots()
                .expect("pnl snapshots should list")
                .len(),
            1
        );
        assert_eq!(
            persistence
                .list_trade_summaries()
                .expect("trade summaries should list")
                .len(),
            1
        );

        let _ = fs::remove_file(sqlite_path);
    }

    #[test]
    fn runtime_persistence_activates_sqlite_when_configured_without_primary() {
        let sqlite_path = unique_temp_path("tv_bot_runtime_sqlite_only", "db");
        let config = load_test_config(&[
            ("TV_BOT__RUNTIME__STARTUP_MODE", "paper"),
            ("TV_BOT__RUNTIME__ALLOW_SQLITE_FALLBACK", "true"),
            ("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED", "true"),
            (
                "TV_BOT__PERSISTENCE__SQLITE_FALLBACK_PATH",
                sqlite_path.to_str().expect("path should be utf8"),
            ),
        ])
        .expect("config should load");

        let runtime = RuntimePersistence::open(&config);
        let selection = runtime.selection();

        assert_eq!(selection.active_backend, PersistenceBackendKind::Sqlite);
        assert!(selection.durable);
        assert!(!selection.fallback_activated);

        let _ = fs::remove_file(sqlite_path);
    }

    #[test]
    fn runtime_persistence_falls_back_to_sqlite_when_primary_is_unavailable() {
        let sqlite_path = unique_temp_path("tv_bot_runtime_sqlite_fallback", "db");
        let config = load_test_config(&[
            ("TV_BOT__RUNTIME__STARTUP_MODE", "paper"),
            ("TV_BOT__RUNTIME__ALLOW_SQLITE_FALLBACK", "true"),
            (
                "TV_BOT__PERSISTENCE__PRIMARY_URL",
                "postgres://postgres@127.0.0.1:1/tv_bot?connect_timeout=1",
            ),
            ("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED", "true"),
            (
                "TV_BOT__PERSISTENCE__SQLITE_FALLBACK_PATH",
                sqlite_path.to_str().expect("path should be utf8"),
            ),
        ])
        .expect("config should load");

        let runtime = RuntimePersistence::open(&config);
        let selection = runtime.selection();

        assert_eq!(selection.active_backend, PersistenceBackendKind::Sqlite);
        assert!(selection.durable);
        assert!(selection.fallback_activated);
        assert!(selection.detail.contains("SQLite fallback is active"));

        let _ = fs::remove_file(sqlite_path);
    }

    #[test]
    fn runtime_persistence_falls_back_to_in_memory_when_sqlite_cannot_open() {
        let blocked_parent = unique_temp_path("tv_bot_runtime_blocked_parent", "txt");
        fs::write(&blocked_parent, b"not a directory").expect("blocked file should write");
        let sqlite_path = blocked_parent.join("tv_bot.sqlite");
        let config = load_test_config(&[
            ("TV_BOT__RUNTIME__STARTUP_MODE", "paper"),
            ("TV_BOT__PERSISTENCE__SQLITE_FALLBACK_ENABLED", "true"),
            (
                "TV_BOT__PERSISTENCE__SQLITE_FALLBACK_PATH",
                sqlite_path.to_str().expect("path should be utf8"),
            ),
        ])
        .expect("config should load");

        let runtime = RuntimePersistence::open(&config);
        let selection = runtime.selection();

        assert_eq!(selection.active_backend, PersistenceBackendKind::InMemory);
        assert!(!selection.durable);
        assert!(selection.detail.contains("in-memory persistence only"));

        let _ = fs::remove_file(blocked_parent);
    }
}
