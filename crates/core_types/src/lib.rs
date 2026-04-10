//! Shared domain contracts for the trading runtime.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Paper,
    Live,
    Observation,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveRuntimeMode {
    Paper,
    Live,
    Observation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarmupStatus {
    NotLoaded,
    Loaded,
    Warming,
    Ready,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArmState {
    Disarmed,
    Armed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractMode {
    FrontMonthAuto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Timeframe {
    #[serde(rename = "1s")]
    OneSecond,
    #[serde(rename = "1m")]
    OneMinute,
    #[serde(rename = "5m")]
    FiveMinute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedType {
    Trades,
    #[serde(rename = "ohlcv_1s")]
    Ohlcv1s,
    #[serde(rename = "ohlcv_1m")]
    Ohlcv1m,
    #[serde(rename = "ohlcv_5m")]
    Ohlcv5m,
    Mbp,
    Mbo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    Always,
    FixedWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlattenRuleMode {
    None,
    ByTime,
    SessionEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalCombinationMode {
    All,
    Any,
    NOfM,
    WeightedScore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryOrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSizingMode {
    Fixed,
    RiskBased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReversalMode {
    FlattenFirst,
    DirectReverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerPreference {
    BrokerRequired,
    BrokerPreferred,
    BotAllowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabentoSymbology {
    RawSymbol,
    Parent,
    Continuous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontMonthSelectionBasis {
    FirstNoticeDate,
    LastTradeDate,
    ChainOrder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSide {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalDirection {
    Long,
    Short,
    Flat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskDecisionStatus {
    Accepted,
    Rejected,
    RequiresOverride,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOrderStatus {
    Pending,
    Working,
    Filled,
    Cancelled,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionSource {
    Dashboard,
    Cli,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessCheckStatus {
    Pass,
    Warning,
    Blocking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerEnvironment {
    Demo,
    Live,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerAccountRouting {
    Paper,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerConnectionState {
    Disconnected,
    Authenticating,
    Authenticated,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerHealth {
    Healthy,
    Initializing,
    Degraded,
    Disconnected,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerSyncState {
    Pending,
    Synchronized,
    Stale,
    Mismatch,
    ReviewRequired,
    Disconnected,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyMetadata {
    pub schema_version: u32,
    pub strategy_id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketSelection {
    pub contract_mode: ContractMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketConfig {
    pub market: String,
    pub selection: MarketSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradeWindow {
    pub start: String,
    pub end: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlattenRule {
    pub mode: FlattenRuleMode,
    pub time: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRules {
    pub mode: SessionMode,
    pub timezone: String,
    pub trade_window: Option<TradeWindow>,
    pub no_new_entries_after: Option<String>,
    pub flatten_rule: Option<FlattenRule>,
    #[serde(default)]
    pub allowed_days: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataFeedRequirement {
    #[serde(rename = "type")]
    pub kind: FeedType,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataFeatureRequirements {
    pub volume: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataRequirements {
    pub feeds: Vec<DataFeedRequirement>,
    pub timeframes: Vec<Timeframe>,
    pub multi_timeframe: bool,
    pub requires: Option<DataFeatureRequirements>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarmupRequirements {
    pub bars_required: BTreeMap<Timeframe, u32>,
    pub ready_requires_all: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractMonth {
    pub year: i32,
    pub month: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FuturesContract {
    pub market_family: String,
    pub display_name: String,
    pub venue: String,
    pub symbol_root: String,
    pub month: ContractMonth,
    pub canonical_symbol: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabentoInstrument {
    pub dataset: String,
    pub symbol: String,
    pub symbology: DatabentoSymbology,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentMapping {
    pub market_family: String,
    pub market_display_name: String,
    pub contract_mode: ContractMode,
    pub resolved_contract: FuturesContract,
    pub databento_symbols: Vec<DatabentoInstrument>,
    pub tradovate_symbol: String,
    pub resolution_basis: FrontMonthSelectionBasis,
    pub resolved_at: DateTime<Utc>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalConfirmation {
    pub mode: SignalCombinationMode,
    pub primary_conditions: Vec<String>,
    pub n_required: Option<u32>,
    #[serde(default)]
    pub secondary_conditions: Vec<String>,
    pub score_threshold: Option<Decimal>,
    pub regime_filter: Option<String>,
    #[serde(default)]
    pub sequence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRules {
    pub long_enabled: bool,
    pub short_enabled: bool,
    pub entry_order_type: EntryOrderType,
    pub entry_conditions: Option<serde_json::Value>,
    pub max_entry_distance_ticks: Option<u32>,
    pub entry_timeout_seconds: Option<u32>,
    pub allow_reentry_same_bar: Option<bool>,
    pub entry_filters: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitRules {
    pub exit_on_opposite_signal: bool,
    pub flatten_on_session_end: bool,
    #[serde(default)]
    pub exit_conditions: Vec<String>,
    pub timeout_exit: Option<bool>,
    pub max_hold_seconds: Option<u32>,
    pub emergency_exit_rules: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionSizing {
    pub mode: PositionSizingMode,
    pub contracts: Option<u32>,
    pub max_risk_usd: Option<Decimal>,
    pub min_contracts: Option<u32>,
    pub max_contracts: Option<u32>,
    pub fallback_fixed_contracts: Option<u32>,
    pub rounding_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScalingConfig {
    pub allow_scale_in: bool,
    pub allow_scale_out: bool,
    pub max_legs: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerPreferences {
    pub stop_loss: BrokerPreference,
    pub take_profit: BrokerPreference,
    pub trailing_stop: BrokerPreference,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSpec {
    pub reversal_mode: ReversalMode,
    pub scaling: ScalingConfig,
    pub broker_preferences: BrokerPreferences,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreakEvenRule {
    pub enabled: bool,
    pub activate_at_ticks: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrailingRule {
    pub enabled: bool,
    pub activate_at_ticks: Option<u32>,
    pub trail_ticks: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialTakeProfitTarget {
    pub at_ticks: u32,
    pub percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartialTakeProfitRule {
    pub enabled: bool,
    #[serde(default)]
    pub targets: Vec<PartialTakeProfitTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradeManagement {
    pub initial_stop_ticks: u32,
    pub take_profit_ticks: u32,
    pub break_even: Option<BreakEvenRule>,
    pub trailing: Option<TrailingRule>,
    pub partial_take_profit: Option<PartialTakeProfitRule>,
    pub post_entry_rules: Option<serde_json::Value>,
    pub time_based_adjustments: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyLossLimit {
    pub broker_side_required: bool,
    pub local_backup_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskLimits {
    pub daily_loss: DailyLossLimit,
    pub max_trades_per_day: u32,
    pub max_consecutive_losses: u32,
    pub max_open_positions: Option<u32>,
    pub max_unrealized_drawdown_usd: Option<Decimal>,
    pub cooldown_after_daily_stop: Option<bool>,
    pub max_notional_exposure: Option<Decimal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailsafeRules {
    pub no_new_entries_on_data_degrade: bool,
    pub pause_on_broker_sync_mismatch: bool,
    pub pause_on_reconnect_until_reviewed: Option<bool>,
    pub kill_on_repeated_order_rejects: Option<bool>,
    pub abnormal_spread_guard: Option<bool>,
    pub clock_sanity_required: Option<bool>,
    pub storage_health_required: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateBehavior {
    pub cooldown_after_loss_s: u32,
    pub max_reentries_per_side: u32,
    pub regime_mode: Option<String>,
    pub memory_reset_rules: Option<serde_json::Value>,
    pub post_win_cooldown_s: Option<u32>,
    pub failed_setup_decay: Option<u32>,
    pub reentry_logic: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardDisplay {
    pub show: Vec<String>,
    pub default_overlay: String,
    #[serde(default)]
    pub debug_panels: Vec<String>,
    pub custom_labels: Option<serde_json::Value>,
    pub preferred_chart_timeframe: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledStrategy {
    pub metadata: StrategyMetadata,
    pub market: MarketConfig,
    pub session: SessionRules,
    pub data_requirements: DataRequirements,
    pub warmup: WarmupRequirements,
    pub signal_confirmation: SignalConfirmation,
    pub entry_rules: EntryRules,
    pub exit_rules: ExitRules,
    pub position_sizing: PositionSizing,
    pub execution: ExecutionSpec,
    pub trade_management: TradeManagement,
    pub risk: RiskLimits,
    pub failsafes: FailsafeRules,
    pub state_behavior: StateBehavior,
    pub dashboard_display: DashboardDisplay,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MarketEvent {
    Trade {
        symbol: String,
        price: Decimal,
        quantity: u64,
        occurred_at: DateTime<Utc>,
    },
    Bar {
        symbol: String,
        timeframe: Timeframe,
        open: Decimal,
        high: Decimal,
        low: Decimal,
        close: Decimal,
        volume: u64,
        closed_at: DateTime<Utc>,
    },
    Heartbeat {
        dataset: String,
        occurred_at: DateTime<Utc>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignalDecision {
    pub strategy_id: String,
    pub direction: SignalDirection,
    pub score: Option<Decimal>,
    pub rationale: Vec<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExecutionIntent {
    Enter {
        side: TradeSide,
        order_type: EntryOrderType,
        quantity: u32,
        protective_brackets_expected: bool,
        reason: String,
    },
    Exit {
        reason: String,
    },
    Flatten {
        reason: String,
    },
    CancelWorkingOrders {
        reason: String,
    },
    ReducePosition {
        quantity: u32,
        reason: String,
    },
    PauseStrategy {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RiskDecision {
    pub status: RiskDecisionStatus,
    pub reason: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrokerOrderCommand {
    PlaceOrder {
        symbol: String,
        side: TradeSide,
        quantity: u32,
        order_type: EntryOrderType,
    },
    CancelOrder {
        broker_order_id: String,
    },
    FlattenPosition {
        symbol: String,
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrokerOrderUpdate {
    pub broker_order_id: String,
    pub symbol: String,
    pub status: BrokerOrderStatus,
    pub filled_quantity: u32,
    pub average_fill_price: Option<Decimal>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrokerPositionSnapshot {
    pub symbol: String,
    pub quantity: i32,
    pub average_price: Option<Decimal>,
    pub realized_pnl: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    pub protective_orders_present: bool,
    pub captured_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrokerFillUpdate {
    pub fill_id: String,
    pub broker_order_id: Option<String>,
    pub symbol: String,
    pub side: TradeSide,
    pub quantity: u32,
    pub price: Decimal,
    pub fee: Option<Decimal>,
    pub commission: Option<Decimal>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrokerAccountSnapshot {
    pub account_id: String,
    pub account_name: Option<String>,
    pub cash_balance: Option<Decimal>,
    pub available_funds: Option<Decimal>,
    pub excess_liquidity: Option<Decimal>,
    pub margin_used: Option<Decimal>,
    pub net_liquidation_value: Option<Decimal>,
    pub realized_pnl: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    pub risk_state: Option<String>,
    pub captured_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerAccountSelection {
    pub provider: String,
    pub account_id: String,
    pub account_name: String,
    pub routing: BrokerAccountRouting,
    pub environment: BrokerEnvironment,
    pub selected_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerStatusSnapshot {
    pub provider: String,
    pub environment: BrokerEnvironment,
    pub connection_state: BrokerConnectionState,
    pub health: BrokerHealth,
    pub sync_state: BrokerSyncState,
    pub selected_account: Option<BrokerAccountSelection>,
    pub reconnect_count: u64,
    pub last_authenticated_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_disconnect_reason: Option<String>,
    pub review_required_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessCheck {
    pub name: String,
    pub status: ReadinessCheckStatus,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmReadinessReport {
    pub mode: RuntimeMode,
    pub checks: Vec<ReadinessCheck>,
    pub risk_summary: String,
    pub hard_override_required: bool,
    pub generated_at: DateTime<Utc>,
}

impl ArmReadinessReport {
    pub fn has_blocking_issues(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == ReadinessCheckStatus::Blocking)
    }

    pub fn is_ready_without_override(&self) -> bool {
        !self.has_blocking_issues() && !self.hard_override_required
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemHealthSnapshot {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub reconnect_count: u64,
    pub db_write_latency_ms: Option<u64>,
    pub queue_lag_ms: Option<u64>,
    pub error_count: u64,
    pub feed_degraded: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventJournalRecord {
    pub event_id: String,
    pub category: String,
    pub action: String,
    pub source: ActionSource,
    pub severity: EventSeverity,
    pub occurred_at: DateTime<Utc>,
    pub payload: serde_json::Value,
}
