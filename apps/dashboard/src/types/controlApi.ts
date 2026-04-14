export type DecimalValue = number | string;

export type ActionSource = "dashboard" | "cli" | "system";
export type EventSeverity = "info" | "warning" | "error";
export type RuntimeMode = "paper" | "live" | "observation" | "paused";
export type ArmState = "armed" | "disarmed";
export type WarmupStatus =
  | "not_loaded"
  | "loaded"
  | "warming"
  | "ready"
  | "failed";
export type ReadinessCheckStatus = "pass" | "warning" | "blocking";
export type RuntimeReconnectDecision =
  | "close_position"
  | "leave_broker_protected"
  | "reattach_bot_management";
export type RuntimeShutdownDecision = "flatten_first" | "leave_broker_protected";
export type ManualCommandSource = "dashboard" | "cli";
export type HttpStatusCode =
  | "Ok"
  | "Conflict"
  | "PreconditionRequired"
  | "InternalServerError";

export interface LoadedStrategySummary {
  path: string;
  title: string | null;
  strategy_id: string;
  name: string;
  version: string;
  market_family: string;
  warning_count: number;
}

export interface RuntimeStorageStatus {
  mode: string;
  primary_configured: boolean;
  sqlite_fallback_enabled: boolean;
  sqlite_path: string;
  allow_runtime_fallback: boolean;
  active_backend: string;
  durable: boolean;
  fallback_activated: boolean;
  detail: string;
}

export interface RuntimeJournalStatus {
  backend: string;
  durable: boolean;
  detail: string;
}

export interface BrokerAccountSelection {
  provider: string;
  account_id: string;
  account_name: string;
  routing: string;
  environment: string;
  selected_at: string;
}

export interface BrokerStatusSnapshot {
  provider: string;
  environment: string;
  connection_state: string;
  health: string;
  sync_state: string;
  selected_account: BrokerAccountSelection | null;
  reconnect_count: number;
  last_authenticated_at: string | null;
  last_heartbeat_at: string | null;
  last_sync_at: string | null;
  last_disconnect_reason: string | null;
  review_required_reason: string | null;
  updated_at: string;
}

export interface FeedStatus {
  instrument_symbol: string;
  feed: string;
  state: string;
  last_event_at: string | null;
  detail: string;
}

export interface BufferStatus {
  symbol: string;
  timeframe: string;
  available_bars: number;
  required_bars: number;
  capacity: number;
  ready: boolean;
}

export interface WarmupProgress {
  status: WarmupStatus;
  ready_requires_all: boolean;
  buffers: BufferStatus[];
  started_at: string | null;
  updated_at: string;
  failure_reason: string | null;
}

export interface MarketDataStatusSnapshot {
  provider: string;
  dataset: string;
  connection_state: string;
  health: string;
  feed_statuses: FeedStatus[];
  warmup: WarmupProgress;
  reconnect_count: number;
  last_heartbeat_at: string | null;
  last_disconnect_reason: string | null;
  updated_at: string;
}

export interface MarketDataServiceSnapshot {
  session: {
    market_data: MarketDataStatusSnapshot;
  };
  warmup_requested: boolean;
  warmup_mode: string | { ReplayFrom: string };
  replay_caught_up: boolean;
  trade_ready: boolean;
  updated_at: string;
}

export interface InstrumentMapping {
  market_family: string;
  market_display_name: string;
  contract_mode: string;
  tradovate_symbol: string;
  summary: string;
}

export interface RuntimeReconnectReviewStatus {
  required: boolean;
  reason: string | null;
  last_decision: RuntimeReconnectDecision | null;
  open_position_count: number;
  working_order_count: number;
}

export interface RuntimeShutdownReviewStatus {
  pending_signal: boolean;
  blocked: boolean;
  awaiting_flatten: boolean;
  decision: RuntimeShutdownDecision | null;
  reason: string | null;
  open_position_count: number;
  all_positions_broker_protected: boolean;
}

export interface TradePathLatencySnapshot {
  signal_latency_ms: number | null;
  decision_latency_ms: number | null;
  order_send_latency_ms: number | null;
  broker_ack_latency_ms: number | null;
  fill_latency_ms: number | null;
  sync_update_latency_ms: number | null;
  end_to_end_fill_latency_ms: number | null;
}

export interface TradePathLatencyRecord {
  action_id: string;
  strategy_id: string | null;
  recorded_at: string;
  latency: TradePathLatencySnapshot;
}

export interface SystemHealthSnapshot {
  cpu_percent: number | null;
  memory_bytes: number | null;
  reconnect_count: number;
  db_write_latency_ms: number | null;
  queue_lag_ms: number | null;
  error_count: number;
  feed_degraded: boolean;
  updated_at: string;
}

export interface RuntimeStatusSnapshot {
  mode: RuntimeMode;
  arm_state: ArmState;
  warmup_status: WarmupStatus;
  strategy_loaded: boolean;
  hard_override_active: boolean;
  operator_new_entries_enabled: boolean;
  operator_new_entries_reason: string | null;
  current_strategy: LoadedStrategySummary | null;
  broker_status: BrokerStatusSnapshot | null;
  market_data_status: MarketDataServiceSnapshot | null;
  market_data_detail: string | null;
  storage_status: RuntimeStorageStatus;
  journal_status: RuntimeJournalStatus;
  system_health: SystemHealthSnapshot | null;
  latest_trade_latency: TradePathLatencyRecord | null;
  recorded_trade_latency_count: number;
  current_account_name: string | null;
  instrument_mapping: InstrumentMapping | null;
  instrument_resolution_error: string | null;
  reconnect_review: RuntimeReconnectReviewStatus;
  shutdown_review: RuntimeShutdownReviewStatus;
  http_bind: string;
  websocket_bind: string;
  command_dispatch_ready: boolean;
  command_dispatch_detail: string;
}

export interface ReadinessCheck {
  name: string;
  status: ReadinessCheckStatus;
  message: string;
}

export interface ArmReadinessReport {
  mode: RuntimeMode;
  checks: ReadinessCheck[];
  risk_summary: string;
  hard_override_required: boolean;
  generated_at: string;
}

export interface RuntimeReadinessSnapshot {
  status: RuntimeStatusSnapshot;
  report: ArmReadinessReport;
}

export interface PositionRecord {
  record_id: string;
  strategy_id: string | null;
  run_id: string | null;
  account_id: string | null;
  symbol: string;
  quantity: number;
  average_price: DecimalValue | null;
  realized_pnl: DecimalValue | null;
  unrealized_pnl: DecimalValue | null;
  protective_orders_present: boolean;
  captured_at: string;
}

export interface OrderRecord {
  broker_order_id: string;
  strategy_id: string | null;
  run_id: string | null;
  account_id: string | null;
  symbol: string;
  side: "buy" | "sell";
  order_type: string | null;
  quantity: number;
  filled_quantity: number;
  average_fill_price: DecimalValue | null;
  status: string;
  provider: string;
  submitted_at: string;
  updated_at: string;
}

export interface FillRecord {
  fill_id: string;
  broker_order_id: string | null;
  strategy_id: string | null;
  run_id: string | null;
  account_id: string | null;
  symbol: string;
  side: "buy" | "sell";
  quantity: number;
  price: DecimalValue;
  fee: DecimalValue;
  commission: DecimalValue;
  occurred_at: string;
}

export type TradeSummaryStatus = "open" | "closed" | "cancelled";

export interface TradeSummaryRecord {
  trade_id: string;
  strategy_id: string | null;
  run_id: string | null;
  account_id: string | null;
  symbol: string;
  side: "buy" | "sell";
  status: TradeSummaryStatus;
  quantity: number;
  average_entry_price: DecimalValue;
  average_exit_price: DecimalValue | null;
  opened_at: string;
  closed_at: string | null;
  gross_pnl: DecimalValue;
  net_pnl: DecimalValue;
  fees: DecimalValue;
  commissions: DecimalValue;
  slippage: DecimalValue;
}

export interface PnlSnapshotRecord {
  snapshot_id: string;
  strategy_id: string | null;
  run_id: string | null;
  account_id: string | null;
  symbol: string | null;
  gross_pnl: DecimalValue;
  net_pnl: DecimalValue;
  fees: DecimalValue;
  commissions: DecimalValue;
  slippage: DecimalValue;
  realized_pnl: DecimalValue | null;
  unrealized_pnl: DecimalValue | null;
  captured_at: string;
}

export interface ProjectedTradingHistoryState {
  total_strategy_run_records: number;
  total_order_records: number;
  total_fill_records: number;
  total_position_records: number;
  total_pnl_snapshot_records: number;
  total_trade_summary_records: number;
  active_run_ids: string[];
  orders: Record<string, OrderRecord>;
  working_order_ids: string[];
  fills: Record<string, FillRecord>;
  trade_summaries: Record<string, TradeSummaryRecord>;
  open_position_symbols: string[];
  open_trade_ids: string[];
  latest_order: OrderRecord | null;
  latest_fill: FillRecord | null;
  latest_position: PositionRecord | null;
  latest_pnl_snapshot: PnlSnapshotRecord | null;
  latest_trade_summary: TradeSummaryRecord | null;
  closed_trade_count: number;
  cancelled_trade_count: number;
  closed_trade_gross_pnl: DecimalValue;
  closed_trade_net_pnl: DecimalValue;
  closed_trade_fees: DecimalValue;
  closed_trade_commissions: DecimalValue;
  closed_trade_slippage: DecimalValue;
  recorded_fill_fees: DecimalValue;
  recorded_fill_commissions: DecimalValue;
  last_activity_at: string | null;
}

export interface RuntimeHistorySnapshot {
  projection: ProjectedTradingHistoryState;
}

export interface RuntimeJournalSnapshot {
  total_records: number;
  records: EventJournalRecord[];
}

export type RuntimeStrategyIssueSeverity = "error" | "warning";

export interface RuntimeStrategyIssue {
  severity: RuntimeStrategyIssueSeverity;
  message: string;
  section: string | null;
  field: string | null;
  line: number | null;
}

export interface RuntimeStrategyCatalogEntry {
  path: string;
  display_path: string;
  valid: boolean;
  title: string | null;
  strategy_id: string | null;
  name: string | null;
  version: string | null;
  market_family: string | null;
  warning_count: number;
  error_count: number;
}

export interface RuntimeStrategyLibraryResponse {
  scanned_roots: string[];
  strategies: RuntimeStrategyCatalogEntry[];
}

export interface RuntimeStrategyValidationRequest {
  source: ManualCommandSource;
  path: string;
}

export interface RuntimeStrategyUploadRequest {
  source: ManualCommandSource;
  filename: string;
  markdown: string;
}

export interface RuntimeStrategyValidationResponse {
  path: string;
  display_path: string;
  valid: boolean;
  title: string | null;
  summary: LoadedStrategySummary | null;
  warnings: RuntimeStrategyIssue[];
  errors: RuntimeStrategyIssue[];
}

export type RuntimeSettingsPersistenceMode = "session_only" | "config_file";

export interface RuntimeEditableSettings {
  startup_mode: RuntimeMode;
  default_strategy_path: string | null;
  allow_sqlite_fallback: boolean;
  paper_account_name: string | null;
  live_account_name: string | null;
}

export interface RuntimeSettingsSnapshot {
  editable: RuntimeEditableSettings;
  http_bind: string;
  websocket_bind: string;
  config_file_path: string | null;
  persistence_mode: RuntimeSettingsPersistenceMode;
  restart_required: boolean;
  detail: string;
}

export interface RuntimeSettingsUpdateRequest {
  source: ManualCommandSource;
  settings: RuntimeEditableSettings;
}

export interface RuntimeSettingsUpdateResponse {
  message: string;
  settings: RuntimeSettingsSnapshot;
}

export interface EventJournalRecord {
  event_id: string;
  category: string;
  action: string;
  source: ActionSource;
  severity: EventSeverity;
  occurred_at: string;
  payload: unknown;
}

export interface RuntimeHostHealthResponse {
  status: string;
  system_health: SystemHealthSnapshot | null;
  latest_trade_latency: TradePathLatencyRecord | null;
}

export type RuntimeLifecycleCommand =
  | {
      kind: "set_mode";
      mode: RuntimeMode;
    }
  | {
      kind: "load_strategy";
      path: string;
    }
  | {
      kind: "start_warmup";
    }
  | {
      kind: "mark_warmup_ready";
    }
  | {
      kind: "mark_warmup_failed";
      reason: string | null;
    }
  | {
      kind: "arm";
      allow_override: boolean;
    }
  | {
      kind: "disarm";
    }
  | {
      kind: "pause";
    }
  | {
      kind: "resume";
    }
  | {
      kind: "set_new_entries_enabled";
      enabled: boolean;
      reason: string | null;
    }
  | {
      kind: "resolve_reconnect_review";
      decision: RuntimeReconnectDecision;
      contract_id: number | null;
      reason: string | null;
    }
  | {
      kind: "shutdown";
      decision: RuntimeShutdownDecision;
      contract_id: number | null;
      reason: string | null;
    }
  | {
      kind: "close_position";
      contract_id: number | null;
      reason: string | null;
    }
  | {
      kind: "manual_entry";
      side: "buy" | "sell";
      quantity: number;
      tick_size: DecimalValue;
      entry_reference_price: DecimalValue;
      tick_value_usd: DecimalValue | null;
      reason: string | null;
    }
  | {
      kind: "cancel_working_orders";
      reason: string | null;
    }
  | {
      kind: "flatten";
      contract_id: number;
      reason: string;
    };

export interface RuntimeLifecycleRequest {
  source: ManualCommandSource;
  command: RuntimeLifecycleCommand;
}

export interface ControlApiCommandResult {
  status: string;
  risk_status: string;
  dispatch_performed: boolean;
  reason: string;
  warnings: string[];
}

export interface RuntimeLifecycleResponse {
  status_code: HttpStatusCode;
  message: string;
  status: RuntimeStatusSnapshot;
  readiness: RuntimeReadinessSnapshot;
  command_result: ControlApiCommandResult | null;
}

export type ControlApiEvent =
  | {
      kind: "command_result";
      source: ActionSource;
      result: ControlApiCommandResult;
      occurred_at: string;
    }
  | {
      kind: "readiness_report";
      report: ArmReadinessReport;
      occurred_at: string;
    }
  | {
      kind: "broker_status";
      snapshot: BrokerStatusSnapshot;
      occurred_at: string;
    }
  | {
      kind: "system_health";
      snapshot: SystemHealthSnapshot;
      occurred_at: string;
    }
  | {
      kind: "trade_latency";
      record: TradePathLatencyRecord;
      occurred_at: string;
    }
  | {
      kind: "history_snapshot";
      projection: ProjectedTradingHistoryState;
      occurred_at: string;
    }
  | {
      kind: "journal_record";
      record: EventJournalRecord;
    };
