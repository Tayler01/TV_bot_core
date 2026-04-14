import {
  startTransition,
  useEffect,
  useEffectEvent,
  useRef,
  useState,
  type ReactNode,
} from "react";

import {
  controlApiEventsUrl,
  loadDashboardSnapshot,
  loadStrategyLibrary,
  parseControlApiEvent,
  sendLifecycleCommand,
  updateRuntimeSettings,
  uploadStrategyMarkdown,
  validateStrategyPath,
  type DashboardSnapshot,
  type LifecycleCommandResult,
} from "./lib/api";
import {
  formatCurrency,
  formatDateTime,
  formatDecimal,
  formatInteger,
  formatLatency,
  formatMode,
  formatSignedCurrency,
  formatWarmupMode,
} from "./lib/format";
import type {
  ControlApiEvent,
  DecimalValue,
  EventJournalRecord,
  FillRecord,
  OrderRecord,
  PnlSnapshotRecord,
  ReadinessCheckStatus,
  RuntimeLifecycleCommand,
  RuntimeLifecycleResponse,
  RuntimeMode,
  RuntimeEditableSettings,
  RuntimeReconnectReviewStatus,
  RuntimeShutdownReviewStatus,
  RuntimeSettingsSnapshot,
  RuntimeStatusSnapshot,
  RuntimeStrategyCatalogEntry,
  RuntimeStrategyLibraryResponse,
  RuntimeStrategyValidationResponse,
  TradePathLatencyRecord,
  TradeSummaryRecord,
} from "./types/controlApi";

const REFRESH_INTERVAL_MS = 5_000;
const EVENTS_RECONNECT_DELAY_MS = 1_500;
const MAX_RECENT_EVENTS = 12;
const MAX_RECENT_TRADES = 6;
const MAX_RECENT_JOURNAL_RECORDS = 12;

async function readStrategyUploadFile(file: File): Promise<string> {
  if (typeof file.text === "function") {
    return await file.text();
  }

  if (typeof FileReader !== "undefined") {
    return await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        resolve(typeof reader.result === "string" ? reader.result : "");
      };
      reader.onerror = () => {
        reject(reader.error ?? new Error("Dashboard failed to read the selected strategy file."));
      };
      reader.readAsText(file);
    });
  }

  return String(file);
}

type LoadState = "idle" | "loading" | "ready" | "error";
type BannerTone = "healthy" | "warning" | "danger" | "info";
type EventConnectionState = "connecting" | "open" | "closed" | "error" | "unsupported";

interface ViewModel {
  snapshot: DashboardSnapshot | null;
  loadState: LoadState;
  error: string | null;
  lastAttemptedAt: string | null;
}

interface CommandFeedback {
  tone: BannerTone;
  message: string;
}

interface CommandOptions {
  confirmMessage?: string;
  pendingLabel: string;
}

interface StrategySummaryViewModel {
  library: RuntimeStrategyLibraryResponse | null;
  validation: RuntimeStrategyValidationResponse | null;
  libraryError: string | null;
  validationError: string | null;
  libraryState: LoadState;
  validationState: LoadState;
  selectedPath: string;
}

interface EventFeedItem {
  id: string;
  headline: string;
  detail: string;
  tone: BannerTone;
  occurredAt: string;
}

interface EventFeedViewModel {
  connectionState: EventConnectionState;
  recentEvents: EventFeedItem[];
  lastEventAt: string | null;
  error: string | null;
}

interface RuntimeSettingsDraft {
  startupMode: RuntimeMode;
  defaultStrategyPath: string;
  allowSqliteFallback: boolean;
  paperAccountName: string;
  liveAccountName: string;
}

interface TradePerformanceViewModel {
  closedCount: number;
  openCount: number;
  winRate: number | null;
  averageNet: number | null;
  averageHoldMinutes: number | null;
  largestWin: number | null;
  largestLoss: number | null;
  floatingNet: number | null;
}

interface PnlChartPoint {
  id: string;
  label: string;
  note: string;
  value: number;
  xPercent: number;
  yPercent: number;
  tone: BannerTone;
}

interface PnlChartViewModel {
  points: PnlChartPoint[];
  zeroPercent: number | null;
}

interface PerTradePnlViewModel {
  tradeId: string;
  symbol: string;
  side: TradeSummaryRecord["side"];
  quantity: number;
  status: TradeSummaryRecord["status"];
  netPnl: number | null;
  grossPnl: number | null;
  fees: number | null;
  commissions: number | null;
  slippage: number | null;
  holdMinutes: number | null;
  openedAt: string;
  closedAt: string | null;
  tone: BannerTone;
}

interface JournalCategoryCount {
  category: string;
  count: number;
}

interface JournalSummaryViewModel {
  infoCount: number;
  warningCount: number;
  errorCount: number;
  dashboardCount: number;
  systemCount: number;
  cliCount: number;
  categories: JournalCategoryCount[];
}

interface LatencyStageViewModel {
  key: string;
  label: string;
  value: number | null;
  barPercent: number;
}

const INITIAL_VIEW_MODEL: ViewModel = {
  snapshot: null,
  loadState: "idle",
  error: null,
  lastAttemptedAt: null,
};

const INITIAL_STRATEGY_VIEW_MODEL: StrategySummaryViewModel = {
  library: null,
  validation: null,
  libraryError: null,
  validationError: null,
  libraryState: "idle",
  validationState: "idle",
  selectedPath: "",
};

const INITIAL_EVENT_FEED_VIEW_MODEL: EventFeedViewModel = {
  connectionState: "connecting",
  recentEvents: [],
  lastEventAt: null,
  error: null,
};

function modeTone(mode: RuntimeMode): "paper" | "live" | "neutral" {
  switch (mode) {
    case "paper":
      return "paper";
    case "live":
      return "live";
    default:
      return "neutral";
  }
}

function statusTone(status: ReadinessCheckStatus | BannerTone) {
  switch (status) {
    case "pass":
    case "healthy":
      return "healthy";
    case "warning":
      return "warning";
    case "blocking":
    case "danger":
      return "danger";
    default:
      return "info";
  }
}

function feedbackToneFromHttpStatus(httpStatus: number): BannerTone {
  if (httpStatus >= 500) {
    return "danger";
  }

  if (httpStatus === 409 || httpStatus === 428) {
    return "warning";
  }

  return "healthy";
}

function humanMemory(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "Unavailable";
  }

  const gibibytes = value / 1024 / 1024 / 1024;
  return `${gibibytes.toFixed(2)} GiB`;
}

function reviewTone(review: RuntimeReconnectReviewStatus | RuntimeShutdownReviewStatus) {
  if ("required" in review) {
    return review.required ? "warning" : "healthy";
  }

  return review.blocked || review.awaiting_flatten ? "warning" : "healthy";
}

function latestLatency(status: RuntimeStatusSnapshot) {
  return status.latest_trade_latency?.latency.end_to_end_fill_latency_ms ?? null;
}

function reviewSummary(status: RuntimeStatusSnapshot) {
  if (status.reconnect_review.required) {
    return "Reconnect review required";
  }

  if (status.shutdown_review.blocked || status.shutdown_review.awaiting_flatten) {
    return "Shutdown review pending";
  }

  return "No active safety review";
}

function mergeLifecycleResponseIntoSnapshot(
  snapshot: DashboardSnapshot | null,
  response: RuntimeLifecycleResponse,
): DashboardSnapshot | null {
  if (!snapshot) {
    return null;
  }

  return {
    ...snapshot,
    fetchedAt: new Date().toISOString(),
    status: response.status,
    readiness: response.readiness,
  };
}

function selectStrategyPath(
  library: RuntimeStrategyLibraryResponse | null,
  currentPath: string,
): string {
  if (!library || library.strategies.length === 0) {
    return "";
  }

  if (library.strategies.some((entry) => entry.path === currentPath)) {
    return currentPath;
  }

  return library.strategies.find((entry) => entry.valid)?.path ?? library.strategies[0]?.path ?? "";
}

function strategyTone(entry: RuntimeStrategyCatalogEntry | null | undefined): BannerTone {
  if (!entry) {
    return "info";
  }

  if (!entry.valid) {
    return "danger";
  }

  if (entry.warning_count > 0) {
    return "warning";
  }

  return "healthy";
}

function validationTone(validation: RuntimeStrategyValidationResponse | null): BannerTone {
  if (!validation) {
    return "info";
  }

  if (!validation.valid) {
    return "danger";
  }

  if (validation.warnings.length > 0) {
    return "warning";
  }

  return "healthy";
}

function settingsDraftFromSnapshot(settings: RuntimeSettingsSnapshot): RuntimeSettingsDraft {
  return {
    startupMode: settings.editable.startup_mode,
    defaultStrategyPath: settings.editable.default_strategy_path ?? "",
    allowSqliteFallback: settings.editable.allow_sqlite_fallback,
    paperAccountName: settings.editable.paper_account_name ?? "",
    liveAccountName: settings.editable.live_account_name ?? "",
  };
}

function runtimeSettingsRequestFromDraft(draft: RuntimeSettingsDraft): RuntimeEditableSettings {
  const defaultStrategyPath = draft.defaultStrategyPath.trim();
  const paperAccountName = draft.paperAccountName.trim();
  const liveAccountName = draft.liveAccountName.trim();

  return {
    startup_mode: draft.startupMode,
    default_strategy_path: defaultStrategyPath.length > 0 ? defaultStrategyPath : null,
    allow_sqlite_fallback: draft.allowSqliteFallback,
    paper_account_name: paperAccountName.length > 0 ? paperAccountName : null,
    live_account_name: liveAccountName.length > 0 ? liveAccountName : null,
  };
}

function strategyLabel(validation: RuntimeStrategyValidationResponse | null): string {
  if (!validation) {
    return "No strategy selected";
  }

  if (validation.summary) {
    return `${validation.summary.name} v${validation.summary.version}`;
  }

  return validation.title ?? validation.display_path;
}

function reviewButtonDisabled(
  pendingAction: string | null,
  snapshot: DashboardSnapshot | null,
): boolean {
  return pendingAction !== null || snapshot?.status.command_dispatch_ready !== true;
}

function eventItemTone(event: ControlApiEvent): BannerTone {
  switch (event.kind) {
    case "command_result":
      return event.result.status === "rejected"
        ? "warning"
        : event.result.status === "requires_override"
          ? "warning"
          : "healthy";
    case "readiness_report":
      return event.report.hard_override_required ? "warning" : "info";
    case "system_health":
      return event.snapshot.error_count > 0 || event.snapshot.feed_degraded ? "warning" : "healthy";
    case "trade_latency":
      return "info";
    case "history_snapshot":
      return "info";
    case "broker_status":
      return event.snapshot.health === "healthy" ? "healthy" : "warning";
    case "journal_record":
      return event.record.severity === "error"
        ? "danger"
        : event.record.severity === "warning"
          ? "warning"
          : "info";
  }
}

function eventOccurredAt(event: ControlApiEvent): string {
  return event.kind === "journal_record" ? event.record.occurred_at : event.occurred_at;
}

function compactJson(value: unknown): string {
  if (value === null || value === undefined) {
    return "No payload";
  }

  try {
    return JSON.stringify(value);
  } catch {
    return "Payload unavailable";
  }
}

function prettyJson(value: unknown): string {
  if (value === null || value === undefined) {
    return "No payload";
  }

  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "Payload unavailable";
  }
}

function decimalToNumber(value: DecimalValue | null | undefined): number | null {
  if (value === null || value === undefined) {
    return null;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) ? value : null;
  }

  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function formatPercentage(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) {
    return "Unavailable";
  }

  return `${value.toFixed(1)}%`;
}

function minutesBetween(startAt: string | null | undefined, endAt: string | null | undefined): number | null {
  if (!startAt || !endAt) {
    return null;
  }

  const start = new Date(startAt).getTime();
  const end = new Date(endAt).getTime();
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) {
    return null;
  }

  return (end - start) / 60_000;
}

function formatDurationMinutes(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) {
    return "Unavailable";
  }

  if (value < 60) {
    return `${value.toFixed(value >= 10 ? 0 : 1)} min`;
  }

  const hours = value / 60;
  return `${hours.toFixed(hours >= 10 ? 0 : 1)} hr`;
}

function eventHeadline(event: ControlApiEvent): string {
  switch (event.kind) {
    case "command_result":
      return `Command ${event.result.status}`;
    case "readiness_report":
      return "Readiness report updated";
    case "system_health":
      return "System health updated";
    case "trade_latency":
      return "Trade latency recorded";
    case "history_snapshot":
      return "History projection updated";
    case "broker_status":
      return "Broker status updated";
    case "journal_record":
      return `${event.record.category}:${event.record.action}`;
  }
}

function eventDetail(event: ControlApiEvent): string {
  switch (event.kind) {
    case "command_result":
      return event.result.reason;
    case "readiness_report":
      return event.report.risk_summary;
    case "system_health":
      return `Errors ${formatInteger(event.snapshot.error_count)} | Feed degraded ${
        event.snapshot.feed_degraded ? "yes" : "no"
      }`;
    case "trade_latency":
      return `End-to-end fill ${formatLatency(
        event.record.latency.end_to_end_fill_latency_ms,
      )}`;
    case "history_snapshot":
      return `Open trades ${formatInteger(event.projection.open_trade_ids.length)} | Closed trades ${formatInteger(
        event.projection.closed_trade_count,
      )}`;
    case "broker_status":
      return `${formatMode(event.snapshot.sync_state)} | ${event.snapshot.provider}`;
    case "journal_record":
      return compactJson(event.record.payload);
  }
}

function toEventFeedItem(event: ControlApiEvent): EventFeedItem {
  const occurredAt = eventOccurredAt(event);
  return {
    id:
      event.kind === "journal_record"
        ? event.record.event_id
        : `${event.kind}-${occurredAt}-${eventHeadline(event)}`,
    headline: eventHeadline(event),
    detail: eventDetail(event),
    tone: eventItemTone(event),
    occurredAt,
  };
}

function mergeEventIntoSnapshot(
  snapshot: DashboardSnapshot | null,
  event: ControlApiEvent,
): DashboardSnapshot | null {
  if (!snapshot) {
    return snapshot;
  }

  switch (event.kind) {
    case "readiness_report":
      return {
        ...snapshot,
        fetchedAt: new Date().toISOString(),
        readiness: {
          ...snapshot.readiness,
          report: event.report,
        },
      };
    case "system_health":
      return {
        ...snapshot,
        fetchedAt: new Date().toISOString(),
        status: {
          ...snapshot.status,
          system_health: event.snapshot,
        },
        health: {
          ...snapshot.health,
          system_health: event.snapshot,
        },
      };
    case "trade_latency":
      return {
        ...snapshot,
        fetchedAt: new Date().toISOString(),
        status: {
          ...snapshot.status,
          latest_trade_latency: event.record,
        },
        health: {
          ...snapshot.health,
          latest_trade_latency: event.record,
        },
      };
    case "history_snapshot":
      return {
        ...snapshot,
        fetchedAt: new Date().toISOString(),
        history: {
          projection: event.projection,
        },
      };
    case "journal_record": {
      const records = [
        event.record,
        ...snapshot.journal.records.filter((record) => record.event_id !== event.record.event_id),
      ].slice(0, Math.max(snapshot.journal.records.length, MAX_RECENT_JOURNAL_RECORDS));

      return {
        ...snapshot,
        fetchedAt: new Date().toISOString(),
        journal: {
          total_records: Math.max(snapshot.journal.total_records, records.length),
          records,
        },
      };
    }
    default:
      return snapshot;
  }
}

function compareDescendingDate(left: string, right: string): number {
  return new Date(right).getTime() - new Date(left).getTime();
}

function workingOrdersForProjection(snapshot: DashboardSnapshot): OrderRecord[] {
  return snapshot.history.projection.working_order_ids
    .map((orderId) => snapshot.history.projection.orders[orderId])
    .filter((order): order is OrderRecord => Boolean(order))
    .sort((left, right) => compareDescendingDate(left.updated_at, right.updated_at));
}

function allTradeSummariesForProjection(snapshot: DashboardSnapshot): TradeSummaryRecord[] {
  return Object.values(snapshot.history.projection.trade_summaries).sort((left, right) =>
    compareDescendingDate(left.closed_at ?? left.opened_at, right.closed_at ?? right.opened_at),
  );
}

function recentFillsForProjection(snapshot: DashboardSnapshot): FillRecord[] {
  return Object.values(snapshot.history.projection.fills)
    .sort((left, right) => compareDescendingDate(left.occurred_at, right.occurred_at))
    .slice(0, 6);
}

function recentTradeSummariesForProjection(snapshot: DashboardSnapshot): TradeSummaryRecord[] {
  return allTradeSummariesForProjection(snapshot).slice(0, MAX_RECENT_TRADES);
}

function recentJournalRecords(snapshot: DashboardSnapshot): EventJournalRecord[] {
  return snapshot.journal.records.slice(0, MAX_RECENT_JOURNAL_RECORDS);
}

function tradeTone(trade: TradeSummaryRecord): BannerTone {
  if (trade.status === "cancelled") {
    return "warning";
  }

  if (trade.status === "open") {
    return "info";
  }

  const netPnl = decimalToNumber(trade.net_pnl);
  if (netPnl === null || netPnl === 0) {
    return "info";
  }

  return netPnl > 0 ? "healthy" : "danger";
}

function tradePerformanceForProjection(snapshot: DashboardSnapshot): TradePerformanceViewModel {
  const trades = allTradeSummariesForProjection(snapshot);
  const closedTrades = trades.filter((trade) => trade.status === "closed");
  const winningTrades = closedTrades.filter((trade) => (decimalToNumber(trade.net_pnl) ?? 0) > 0);
  const losingTrades = closedTrades.filter((trade) => (decimalToNumber(trade.net_pnl) ?? 0) < 0);
  const averageNet =
    closedTrades.length > 0
      ? closedTrades.reduce((total, trade) => total + (decimalToNumber(trade.net_pnl) ?? 0), 0) /
        closedTrades.length
      : null;
  const holdingDurations = closedTrades
    .map((trade) => minutesBetween(trade.opened_at, trade.closed_at))
    .filter((value): value is number => value !== null);
  const averageHoldMinutes =
    holdingDurations.length > 0
      ? holdingDurations.reduce((total, value) => total + value, 0) / holdingDurations.length
      : null;

  return {
    closedCount: closedTrades.length,
    openCount: trades.filter((trade) => trade.status === "open").length,
    winRate:
      closedTrades.length > 0 ? (winningTrades.length / closedTrades.length) * 100 : null,
    averageNet,
    averageHoldMinutes,
    largestWin: winningTrades.length
      ? Math.max(...winningTrades.map((trade) => decimalToNumber(trade.net_pnl) ?? 0))
      : null,
    largestLoss: losingTrades.length
      ? Math.min(...losingTrades.map((trade) => decimalToNumber(trade.net_pnl) ?? 0))
      : null,
    floatingNet: decimalToNumber(snapshot.history.projection.latest_pnl_snapshot?.net_pnl),
  };
}

function pnlChartForProjection(snapshot: DashboardSnapshot): PnlChartViewModel {
  const closedTrades = Object.values(snapshot.history.projection.trade_summaries)
    .filter((trade) => trade.status === "closed")
    .sort(
      (left, right) =>
        new Date(left.closed_at ?? left.opened_at).getTime() -
        new Date(right.closed_at ?? right.opened_at).getTime(),
    );

  let cumulativeNet = 0;
  const rawPoints = closedTrades.map((trade, index) => {
    cumulativeNet += decimalToNumber(trade.net_pnl) ?? 0;

    return {
      id: trade.trade_id,
      label: `T${index + 1}`,
      note: `${trade.symbol} closed ${formatSignedCurrency(trade.net_pnl)}`,
      value: cumulativeNet,
    };
  });

  const latestPnlSnapshot = snapshot.history.projection.latest_pnl_snapshot;
  if (latestPnlSnapshot) {
    rawPoints.push({
      id: latestPnlSnapshot.snapshot_id,
      label: "Now",
      note: `Floating now at ${formatDateTime(latestPnlSnapshot.captured_at)}`,
      value: decimalToNumber(latestPnlSnapshot.net_pnl) ?? cumulativeNet,
    });
  }

  const trimmedPoints = rawPoints.slice(-8);
  if (!trimmedPoints.length) {
    return {
      points: [],
      zeroPercent: null,
    };
  }

  let minValue = trimmedPoints.reduce(
    (lowest, point) => Math.min(lowest, point.value),
    trimmedPoints[0]?.value ?? 0,
  );
  let maxValue = trimmedPoints.reduce(
    (highest, point) => Math.max(highest, point.value),
    trimmedPoints[0]?.value ?? 0,
  );

  if (minValue === maxValue) {
    const padding = Math.max(Math.abs(maxValue) * 0.2, 1);
    minValue -= padding;
    maxValue += padding;
  }

  const chartLeft = 6;
  const chartRight = 94;
  const chartTop = 8;
  const chartBottom = 92;
  const range = maxValue - minValue;
  const zeroPercent =
    minValue <= 0 && maxValue >= 0
      ? chartBottom - ((0 - minValue) / range) * (chartBottom - chartTop)
      : null;

  return {
    points: trimmedPoints.map((point, index) => ({
      ...point,
      xPercent:
        trimmedPoints.length === 1
          ? 50
          : chartLeft + (index / (trimmedPoints.length - 1)) * (chartRight - chartLeft),
      yPercent: chartBottom - ((point.value - minValue) / range) * (chartBottom - chartTop),
      tone: point.value > 0 ? "healthy" : point.value < 0 ? "danger" : "info",
    })),
    zeroPercent,
  };
}

function pnlChartPath(points: PnlChartPoint[]): string {
  return points
    .map((point, index) =>
      `${index === 0 ? "M" : "L"} ${point.xPercent.toFixed(1)} ${point.yPercent.toFixed(1)}`,
    )
    .join(" ");
}

function perTradePnlForProjection(snapshot: DashboardSnapshot): PerTradePnlViewModel[] {
  return recentTradeSummariesForProjection(snapshot).map((trade) => ({
    tradeId: trade.trade_id,
    symbol: trade.symbol,
    side: trade.side,
    quantity: trade.quantity,
    status: trade.status,
    netPnl: decimalToNumber(trade.net_pnl),
    grossPnl: decimalToNumber(trade.gross_pnl),
    fees: decimalToNumber(trade.fees),
    commissions: decimalToNumber(trade.commissions),
    slippage: decimalToNumber(trade.slippage),
    holdMinutes: minutesBetween(trade.opened_at, trade.closed_at),
    openedAt: trade.opened_at,
    closedAt: trade.closed_at,
    tone: tradeTone(trade),
  }));
}

function journalRecordTone(record: EventJournalRecord): BannerTone {
  if (record.severity === "error") {
    return "danger";
  }

  if (record.severity === "warning") {
    return "warning";
  }

  return "info";
}

function summarizeJournalRecords(records: EventJournalRecord[]): JournalSummaryViewModel {
  const counts = {
    infoCount: 0,
    warningCount: 0,
    errorCount: 0,
    dashboardCount: 0,
    systemCount: 0,
    cliCount: 0,
  };
  const categories = new Map<string, number>();

  for (const record of records) {
    if (record.severity === "error") {
      counts.errorCount += 1;
    } else if (record.severity === "warning") {
      counts.warningCount += 1;
    } else {
      counts.infoCount += 1;
    }

    if (record.source === "dashboard") {
      counts.dashboardCount += 1;
    } else if (record.source === "system") {
      counts.systemCount += 1;
    } else {
      counts.cliCount += 1;
    }

    categories.set(record.category, (categories.get(record.category) ?? 0) + 1);
  }

  return {
    ...counts,
    categories: [...categories.entries()]
      .sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]))
      .slice(0, 4)
      .map(([category, count]) => ({ category, count })),
  };
}

function latestPnlSnapshot(snapshot: DashboardSnapshot): PnlSnapshotRecord | null {
  return snapshot.history.projection.latest_pnl_snapshot;
}

function latencyStages(record: TradePathLatencyRecord | null | undefined): LatencyStageViewModel[] {
  const stages = [
    { key: "signal", label: "Signal", value: record?.latency.signal_latency_ms ?? null },
    { key: "decision", label: "Decision", value: record?.latency.decision_latency_ms ?? null },
    { key: "order-send", label: "Order send", value: record?.latency.order_send_latency_ms ?? null },
    { key: "broker-ack", label: "Broker ack", value: record?.latency.broker_ack_latency_ms ?? null },
    { key: "fill", label: "Fill", value: record?.latency.fill_latency_ms ?? null },
    { key: "sync-update", label: "Sync update", value: record?.latency.sync_update_latency_ms ?? null },
    {
      key: "end-to-end",
      label: "End to end",
      value: record?.latency.end_to_end_fill_latency_ms ?? null,
    },
  ];
  const maxValue = stages.reduce(
    (largest, stage) => Math.max(largest, stage.value ?? 0),
    0,
  );

  return stages.map((stage) => ({
    ...stage,
    barPercent:
      stage.value === null || maxValue === 0
        ? 0
        : Math.max(12, Math.round((stage.value / maxValue) * 100)),
  }));
}

function isPositiveNumberInput(value: string): boolean {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0;
}

function Panel({
  eyebrow,
  title,
  detail,
  children,
  className,
}: {
  eyebrow: string;
  title: string;
  detail?: string;
  children: ReactNode;
  className?: string;
}) {
  const panelClassName = className ? `panel ${className}` : "panel";

  return (
    <section className={panelClassName}>
      <div className="panel__heading">
        <div>
          <p className="eyebrow">{eyebrow}</p>
          <h2>{title}</h2>
        </div>
        {detail ? <p className="panel__detail">{detail}</p> : null}
      </div>
      {children}
    </section>
  );
}

function Pill({
  label,
  tone,
}: {
  label: string;
  tone: "healthy" | "warning" | "danger" | "info";
}) {
  return <span className={`pill pill--${tone}`}>{label}</span>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function MiniMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="mini-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function Definition({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}

function App() {
  const strategyUploadInputRef = useRef<HTMLInputElement | null>(null);
  const settingsDraftRef = useRef<RuntimeSettingsDraft | null>(null);
  const [viewModel, setViewModel] = useState<ViewModel>(INITIAL_VIEW_MODEL);
  const [strategyViewModel, setStrategyViewModel] = useState<StrategySummaryViewModel>(
    INITIAL_STRATEGY_VIEW_MODEL,
  );
  const [eventFeed, setEventFeed] = useState<EventFeedViewModel>(INITIAL_EVENT_FEED_VIEW_MODEL);
  const [commandFeedback, setCommandFeedback] = useState<CommandFeedback | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [newEntriesReason, setNewEntriesReason] = useState("dashboard operator entry gate");
  const [closePositionReason, setClosePositionReason] = useState(
    "dashboard flatten position request",
  );
  const [manualEntrySide, setManualEntrySide] = useState<"buy" | "sell">("buy");
  const [manualEntryQuantity, setManualEntryQuantity] = useState("1");
  const [manualEntryTickSize, setManualEntryTickSize] = useState("0.1");
  const [manualEntryReferencePrice, setManualEntryReferencePrice] = useState("");
  const [manualEntryTickValueUsd, setManualEntryTickValueUsd] = useState("");
  const [manualEntryReason, setManualEntryReason] = useState("dashboard manual entry");
  const [cancelWorkingOrdersReason, setCancelWorkingOrdersReason] = useState(
    "dashboard cancel working orders request",
  );
  const [reconnectReason, setReconnectReason] = useState(
    "dashboard reconnect review resolution",
  );
  const [shutdownReason, setShutdownReason] = useState("dashboard shutdown review decision");
  const [selectedStrategyUploadFile, setSelectedStrategyUploadFile] = useState<File | null>(null);
  const [settingsDraft, setSettingsDraft] = useState<RuntimeSettingsDraft | null>(null);
  const [settingsDirty, setSettingsDirty] = useState(false);

  const refreshSnapshot = useEffectEvent(async (signal?: AbortSignal) => {
    const attemptedAt = new Date().toISOString();

    setViewModel((current) => ({
      ...current,
      loadState: current.snapshot ? "ready" : "loading",
      error: null,
      lastAttemptedAt: attemptedAt,
    }));

    try {
      const snapshot = await loadDashboardSnapshot(signal);
      startTransition(() => {
        setViewModel({
          snapshot,
          loadState: "ready",
          error: null,
          lastAttemptedAt: attemptedAt,
        });
      });
    } catch (error) {
      if (signal?.aborted) {
        return;
      }

      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to read the local control API.";

      setViewModel((current) => ({
        ...current,
        loadState: current.snapshot ? "ready" : "error",
        error: message,
        lastAttemptedAt: attemptedAt,
      }));
    }
  });

  const executeLifecycleCommand = useEffectEvent(
    async (
      command: RuntimeLifecycleCommand,
      options: CommandOptions,
    ): Promise<LifecycleCommandResult | null> => {
      if (options.confirmMessage && !window.confirm(options.confirmMessage)) {
        return null;
      }

      setPendingAction(options.pendingLabel);
      setCommandFeedback(null);

      try {
        const result = await sendLifecycleCommand(command);
        let refreshedSnapshot: DashboardSnapshot | null = null;

        try {
          refreshedSnapshot = await loadDashboardSnapshot();
        } catch {
          refreshedSnapshot = null;
        }

        setViewModel((current) => ({
          ...current,
          snapshot:
            refreshedSnapshot ?? mergeLifecycleResponseIntoSnapshot(current.snapshot, result.response),
          loadState: "ready",
          error: null,
          lastAttemptedAt: new Date().toISOString(),
        }));
        setCommandFeedback({
          tone: feedbackToneFromHttpStatus(result.httpStatus),
          message: result.response.message,
        });

        return result;
      } catch (error) {
        const message =
          error instanceof Error
            ? error.message
            : "Runtime command failed before the dashboard received a valid response.";

        setCommandFeedback({
          tone: "danger",
          message,
        });
        return null;
      } finally {
        setPendingAction(null);
      }
    },
  );

  const executeReconnectDecision = useEffectEvent(
    async (decision: "close_position" | "leave_broker_protected" | "reattach_bot_management") => {
      const confirmMessage =
        decision === "close_position"
          ? "Close the active broker position as part of reconnect recovery?"
          : undefined;

      const result = await executeLifecycleCommand(
        {
          kind: "resolve_reconnect_review",
          decision,
          contract_id: null,
          reason: reconnectReason.trim() || null,
        },
        {
          pendingLabel: `Resolving reconnect review with ${decision}`,
          confirmMessage,
        },
      );

      if (result?.httpStatus === 200) {
        setReconnectReason("dashboard reconnect review resolution");
      }
    },
  );

  const executeShutdownDecision = useEffectEvent(
    async (decision: "flatten_first" | "leave_broker_protected") => {
      const confirmMessage =
        decision === "flatten_first"
          ? "Request flatten-first shutdown handling now? The runtime will flatten and then continue shutdown once the broker position is flat."
          : "Approve shutdown while leaving broker-protected positions in place?";

      const result = await executeLifecycleCommand(
        {
          kind: "shutdown",
          decision,
          contract_id: null,
          reason: shutdownReason.trim() || null,
        },
        {
          pendingLabel: `Submitting shutdown review decision ${decision}`,
          confirmMessage,
        },
      );

      if (result?.httpStatus === 200) {
        setShutdownReason("dashboard shutdown review decision");
      }
    },
  );

  const updateNewEntriesEnabled = useEffectEvent(async (enabled: boolean) => {
    const result = await executeLifecycleCommand(
      {
        kind: "set_new_entries_enabled",
        enabled,
        reason: newEntriesReason.trim() || null,
      },
      {
        pendingLabel: enabled ? "Re-enabling new entries" : "Disabling new entries",
        confirmMessage: enabled
          ? undefined
          : "Disable new entries now? Existing positions can still be managed, but fresh entry requests will stay blocked until you re-enable them.",
      },
    );

    if (result?.httpStatus === 200) {
      setNewEntriesReason("dashboard operator entry gate");
    }
  });

  const refreshStrategyLibrary = useEffectEvent(async (signal?: AbortSignal) => {
    setStrategyViewModel((current) => ({
      ...current,
      libraryState: "loading",
      libraryError: null,
    }));

    try {
      const library = await loadStrategyLibrary(signal);
      setStrategyViewModel((current) => ({
        ...current,
        library,
        libraryState: "ready",
        libraryError: null,
        selectedPath: selectStrategyPath(library, current.selectedPath),
      }));
    } catch (error) {
      if (signal?.aborted) {
        return;
      }

      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to read the local strategy library.";

      setStrategyViewModel((current) => ({
        ...current,
        libraryState: current.library ? "ready" : "error",
        libraryError: message,
      }));
    }
  });

  const saveRuntimeSettings = useEffectEvent(async () => {
    if (!settingsDraft) {
      return;
    }

    setPendingAction("Saving runtime settings");
    setCommandFeedback(null);

    try {
      const result = await updateRuntimeSettings({
        source: "dashboard",
        settings: runtimeSettingsRequestFromDraft(settingsDraft),
      });
      let refreshedSnapshot: DashboardSnapshot | null = null;

      try {
        refreshedSnapshot = await loadDashboardSnapshot();
      } catch {
        refreshedSnapshot = null;
      }

      setViewModel((current) => ({
        ...current,
        snapshot:
          refreshedSnapshot ??
          (current.snapshot
            ? {
                ...current.snapshot,
                settings: result.settings,
                fetchedAt: new Date().toISOString(),
              }
            : null),
        loadState: "ready",
        error: null,
        lastAttemptedAt: new Date().toISOString(),
      }));
      const nextDraft = settingsDraftFromSnapshot(result.settings);
      settingsDraftRef.current = nextDraft;
      setSettingsDraft(nextDraft);
      setSettingsDirty(false);
      setCommandFeedback({
        tone: result.settings.persistence_mode === "config_file" ? "healthy" : "warning",
        message: result.message,
      });
    } catch (error) {
      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to save runtime settings through the local control API.";
      setCommandFeedback({
        tone: "danger",
        message,
      });
    } finally {
      setPendingAction(null);
    }
  });

  const refreshStrategyValidation = useEffectEvent(
    async (path: string, signal?: AbortSignal) => {
      if (!path) {
        setStrategyViewModel((current) => ({
          ...current,
          validation: null,
          validationError: null,
          validationState: "idle",
        }));
        return;
      }

      setStrategyViewModel((current) => ({
        ...current,
        validationState: "loading",
        validationError: null,
      }));

      try {
        const validation = await validateStrategyPath(path, signal);
        setStrategyViewModel((current) => ({
          ...current,
          validation,
          validationError: null,
          validationState: "ready",
        }));
      } catch (error) {
        if (signal?.aborted) {
          return;
        }

        const message =
          error instanceof Error
            ? error.message
            : "Dashboard failed to validate the selected strategy.";

        setStrategyViewModel((current) => ({
          ...current,
          validation: null,
          validationError: message,
          validationState: "error",
        }));
      }
    },
  );

  const uploadSelectedStrategyFile = useEffectEvent(async () => {
    if (!selectedStrategyUploadFile) {
      return;
    }

    setPendingAction("Uploading strategy into the local runtime library");
    setCommandFeedback(null);

    try {
      const markdown = await readStrategyUploadFile(selectedStrategyUploadFile);
      const validation = await uploadStrategyMarkdown(
        selectedStrategyUploadFile.name,
        markdown,
      );

      await refreshStrategyLibrary();
      setStrategyViewModel((current) => ({
        ...current,
        selectedPath: validation.path,
        validation,
        validationError: null,
        validationState: "ready",
      }));
      setSelectedStrategyUploadFile(null);
      if (strategyUploadInputRef.current) {
        strategyUploadInputRef.current.value = "";
      }

      setCommandFeedback({
        tone: validation.valid
          ? validation.warnings.length > 0
            ? "warning"
            : "healthy"
          : "warning",
        message: validation.valid
          ? `Saved uploaded strategy to ${validation.display_path} and validated it through the runtime host.`
          : `Saved uploaded strategy to ${validation.display_path}, but validation found ${validation.errors.length} error(s).`,
      });
    } catch (error) {
      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to upload the selected strategy file.";

      setCommandFeedback({
        tone: "danger",
        message,
      });
    } finally {
      setPendingAction(null);
    }
  });

  useEffect(() => {
    const controller = new AbortController();
    void refreshSnapshot(controller.signal);
    void refreshStrategyLibrary(controller.signal);

    const intervalId = window.setInterval(() => {
      void refreshSnapshot();
    }, REFRESH_INTERVAL_MS);

    return () => {
      controller.abort();
      window.clearInterval(intervalId);
    };
  }, []);

  useEffect(() => {
    if (typeof WebSocket === "undefined") {
      setEventFeed({
        connectionState: "unsupported",
        recentEvents: [],
        lastEventAt: null,
        error: "This environment does not provide WebSocket support.",
      });
      return;
    }

    let active = true;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | null = null;

    const connect = () => {
      if (!active) {
        return;
      }

      setEventFeed((current) => ({
        ...current,
        connectionState: "connecting",
        error: null,
      }));

      socket = new WebSocket(controlApiEventsUrl());

      socket.onopen = () => {
        if (!active) {
          return;
        }

        setEventFeed((current) => ({
          ...current,
          connectionState: "open",
          error: null,
        }));
      };

      socket.onmessage = (message) => {
        if (!active || typeof message.data !== "string") {
          return;
        }

        try {
          const event = parseControlApiEvent(message.data);
          const feedItem = toEventFeedItem(event);

          setEventFeed((current) => ({
            ...current,
            connectionState: "open",
            recentEvents: [feedItem, ...current.recentEvents].slice(0, MAX_RECENT_EVENTS),
            lastEventAt: feedItem.occurredAt,
            error: null,
          }));
          setViewModel((current) => ({
            ...current,
            snapshot: mergeEventIntoSnapshot(current.snapshot, event),
          }));
        } catch (error) {
          const detail =
            error instanceof Error ? error.message : "Dashboard could not parse an event.";
          setEventFeed((current) => ({
            ...current,
            connectionState: "error",
            error: detail,
          }));
        }
      };

      socket.onerror = () => {
        if (!active) {
          return;
        }

        setEventFeed((current) => ({
          ...current,
          connectionState: "error",
          error: "Local event stream reported a transport error.",
        }));
      };

      socket.onclose = () => {
        if (!active) {
          return;
        }

        setEventFeed((current) => ({
          ...current,
          connectionState: "closed",
          error: current.error ?? "Event stream closed; retrying shortly.",
        }));
        reconnectTimer = window.setTimeout(() => {
          reconnectTimer = null;
          connect();
        }, EVENTS_RECONNECT_DELAY_MS);
      };
    };

    connect();

    return () => {
      active = false;
      if (reconnectTimer !== null) {
        window.clearTimeout(reconnectTimer);
      }
      socket?.close();
    };
  }, []);

  useEffect(() => {
    if (!strategyViewModel.selectedPath) {
      return;
    }

    const controller = new AbortController();
    void refreshStrategyValidation(strategyViewModel.selectedPath, controller.signal);

    return () => {
      controller.abort();
    };
  }, [strategyViewModel.selectedPath]);

  const snapshot = viewModel.snapshot;
  const updateSettingsDraft = useEffectEvent(
    (updater: (draft: RuntimeSettingsDraft) => RuntimeSettingsDraft) => {
      if (!snapshot) {
        return;
      }

      setSettingsDirty(true);
      setSettingsDraft((current) => {
        const next = updater(
          current ?? settingsDraftRef.current ?? settingsDraftFromSnapshot(snapshot.settings),
        );
        settingsDraftRef.current = next;
        return next;
      });
    },
  );

  useEffect(() => {
    if (!snapshot || settingsDirty) {
      return;
    }

    const nextDraft = settingsDraftFromSnapshot(snapshot.settings);
    settingsDraftRef.current = nextDraft;
    setSettingsDraft(nextDraft);
  }, [settingsDirty, snapshot]);

  const selectedStrategyEntry =
    strategyViewModel.library?.strategies.find(
      (entry) => entry.path === strategyViewModel.selectedPath,
    ) ?? null;
  const headlineTone = snapshot ? modeTone(snapshot.status.mode) : "neutral";
  const readinessCounts = snapshot
    ? snapshot.readiness.report.checks.reduce(
        (counts, check) => {
          counts[check.status] += 1;
          return counts;
        },
        { pass: 0, warning: 0, blocking: 0 },
      )
    : { pass: 0, warning: 0, blocking: 0 };
  const armButtonLabel = snapshot
    ? snapshot.status.arm_state === "armed"
      ? "Disarm runtime"
      : snapshot.readiness.report.hard_override_required
        ? "Arm with temporary override"
        : "Arm runtime"
    : "Arm runtime";
  const pauseButtonLabel = snapshot?.status.mode === "paused" ? "Resume runtime" : "Pause runtime";
  const openWorkingOrders = snapshot ? workingOrdersForProjection(snapshot) : [];
  const recentFills = snapshot ? recentFillsForProjection(snapshot) : [];
  const recentTrades = snapshot ? recentTradeSummariesForProjection(snapshot) : [];
  const journalRecords = snapshot ? recentJournalRecords(snapshot) : [];
  const tradePerformance = snapshot ? tradePerformanceForProjection(snapshot) : null;
  const pnlChart = snapshot ? pnlChartForProjection(snapshot) : null;
  const pnlChartPathData = pnlChart ? pnlChartPath(pnlChart.points) : "";
  const perTradePnl = snapshot ? perTradePnlForProjection(snapshot) : [];
  const journalSummary = summarizeJournalRecords(journalRecords);
  const latencyBreakdown = snapshot ? latencyStages(snapshot.health.latest_trade_latency) : [];
  const slowestLatencyStage = latencyBreakdown.reduce<LatencyStageViewModel | null>(
    (slowest, stage) => {
      if (stage.value === null) {
        return slowest;
      }

      if (!slowest || (slowest.value ?? -1) < stage.value) {
        return stage;
      }

      return slowest;
    },
    null,
  );
  const projectedPnlSnapshot = snapshot ? latestPnlSnapshot(snapshot) : null;
  const feedStatuses = snapshot?.status.market_data_status?.session.market_data.feed_statuses ?? [];
  const canManualEntry =
    snapshot != null &&
    snapshot.status.strategy_loaded === true &&
    snapshot.status.command_dispatch_ready === true &&
    snapshot.status.operator_new_entries_enabled === true &&
    snapshot.status.arm_state === "armed" &&
    (snapshot.status.mode === "paper" || snapshot.status.mode === "live") &&
    manualEntryReason.trim().length > 0 &&
    isPositiveNumberInput(manualEntryQuantity) &&
    isPositiveNumberInput(manualEntryTickSize) &&
    isPositiveNumberInput(manualEntryReferencePrice) &&
    (manualEntryTickValueUsd.trim().length === 0 ||
      isPositiveNumberInput(manualEntryTickValueUsd));
  const canClosePosition =
    (snapshot?.history.projection.open_position_symbols.length ?? 0) > 0 &&
    closePositionReason.trim().length > 0 &&
    snapshot?.status.command_dispatch_ready === true;
  const canCancelWorkingOrders =
    openWorkingOrders.length > 0 &&
    cancelWorkingOrdersReason.trim().length > 0 &&
    snapshot?.status.command_dispatch_ready === true;
  const canLoadSelectedStrategy =
    strategyViewModel.selectedPath.length > 0 &&
    strategyViewModel.validation?.valid === true &&
    pendingAction === null;
  const canUploadSelectedStrategyFile =
    selectedStrategyUploadFile !== null && pendingAction === null;
  const canDisableNewEntries =
    snapshot != null &&
    pendingAction === null &&
    snapshot.status.operator_new_entries_enabled === true;
  const canEnableNewEntries =
    snapshot != null &&
    pendingAction === null &&
    snapshot.status.operator_new_entries_enabled === false;
  const canSaveSettings =
    snapshot != null && settingsDraft != null && settingsDirty && pendingAction === null;
  const reviewActionsDisabled = reviewButtonDisabled(pendingAction, snapshot);
  const reconnectCloseDisabled =
    reviewActionsDisabled || snapshot?.status.reconnect_review.required !== true;
  const shutdownLeaveDisabled =
    reviewActionsDisabled ||
    snapshot?.status.shutdown_review.blocked !== true ||
    snapshot.status.shutdown_review.all_positions_broker_protected !== true;
  const shutdownFlattenDisabled =
    reviewActionsDisabled || snapshot?.status.shutdown_review.blocked !== true;

  return (
    <main className="shell">
      <div className={`hero hero--${headlineTone}`}>
        <div className="hero__copy">
          <p className="eyebrow">TV Bot Control Center</p>
          <h1>Operator dashboard for the local runtime host</h1>
          <p className="hero__summary">
            This slice adds the first real control-center actions on top of the local control
            plane, while keeping live and paper modes visually distinct and confirming the risky
            paths before the dashboard sends them.
          </p>
        </div>
        <div className="hero__meta">
          <div className="hero__mode-lockup">
            <span className="hero__mode-label">Mode</span>
            <strong>{snapshot ? formatMode(snapshot.status.mode) : "Waiting for runtime"}</strong>
          </div>
          <div className="hero__actions">
            <button
              className="refresh-button"
              type="button"
              onClick={() => {
                void refreshSnapshot();
              }}
            >
              Refresh now
            </button>
            <p className="hero__timestamp">
              Last sync{" "}
              {snapshot ? formatDateTime(snapshot.fetchedAt) : formatDateTime(viewModel.lastAttemptedAt)}
            </p>
          </div>
        </div>
      </div>

      {viewModel.error ? (
        <section className="banner banner--warning" role="status">
          <strong>Local control-plane read failed.</strong>
          <span>{viewModel.error}</span>
        </section>
      ) : null}

      {commandFeedback ? (
        <section className={`banner banner--${commandFeedback.tone}`} role="status">
          <strong>Operator action result.</strong>
          <span>{commandFeedback.message}</span>
        </section>
      ) : null}

      {pendingAction ? (
        <section className="banner banner--info" role="status">
          <strong>Action in progress.</strong>
          <span>{pendingAction}</span>
        </section>
      ) : null}

      {!snapshot && viewModel.loadState !== "error" ? (
        <section className="empty-state" aria-live="polite">
          <h2>Waiting for runtime status</h2>
          <p>The dashboard is polling the local runtime host for its first snapshot.</p>
        </section>
      ) : null}

      {snapshot ? (
        <div className="dashboard-grid">
          <Panel
            className="panel--full"
            eyebrow="Control Center"
            title="Lifecycle commands through /runtime/commands"
            detail={`Current mode: ${formatMode(snapshot.status.mode)} | Dispatch: ${snapshot.status.command_dispatch_detail}`}
          >
            <div className="control-grid">
              <section className="control-card">
                <p className="control-card__title">Mode</p>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null || snapshot.status.mode === "paper"}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "set_mode", mode: "paper" },
                        { pendingLabel: "Switching runtime to paper mode" },
                      );
                    }}
                  >
                    Paper
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null || snapshot.status.mode === "observation"}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "set_mode", mode: "observation" },
                        { pendingLabel: "Switching runtime to observation mode" },
                      );
                    }}
                  >
                    Observation
                  </button>
                  <button
                    className="command-button command-button--danger"
                    type="button"
                    disabled={pendingAction !== null || snapshot.status.mode === "live"}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "set_mode", mode: "live" },
                        {
                          pendingLabel: "Switching runtime to live mode",
                          confirmMessage:
                            "Switch the runtime into LIVE mode? Paper and live are intentionally separated. Continue?",
                        },
                      );
                    }}
                  >
                    Live
                  </button>
                </div>
              </section>

              <section className="control-card">
                <p className="control-card__title">New entry gate</p>
                <div className="pill-row">
                  <Pill
                    label={
                      snapshot.status.operator_new_entries_enabled
                        ? "New entries enabled"
                        : "New entries disabled"
                    }
                    tone={
                      snapshot.status.operator_new_entries_enabled ? "healthy" : "warning"
                    }
                  />
                  <Pill
                    label={
                      snapshot.status.operator_new_entries_reason ??
                      "Operator gate is open for fresh entries"
                    }
                    tone={
                      snapshot.status.operator_new_entries_enabled ? "info" : "warning"
                    }
                  />
                </div>
                <label className="field field--wide">
                  <span>Reason</span>
                  <input
                    aria-label="New entry gate reason"
                    placeholder="dashboard operator entry gate"
                    value={newEntriesReason}
                    onChange={(event) => {
                      setNewEntriesReason(event.target.value);
                    }}
                  />
                </label>
                <div className="action-row">
                  <button
                    className="command-button command-button--danger"
                    type="button"
                    disabled={!canDisableNewEntries}
                    onClick={() => {
                      void updateNewEntriesEnabled(false);
                    }}
                  >
                    Disable new entries
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={!canEnableNewEntries}
                    onClick={() => {
                      void updateNewEntriesEnabled(true);
                    }}
                  >
                    Enable new entries
                  </button>
                </div>
                <p className="control-card__note">
                  This gate blocks fresh entry requests through the runtime host while still
                  leaving flatten, close, and cancel actions available on existing exposure.
                </p>
              </section>

              <section className="control-card control-card--wide">
                <p className="control-card__title">Strategy Library</p>
                <div className="strategy-toolbar">
                  <label className="field field--wide">
                    <span>Available strategy</span>
                    <select
                      aria-label="Available strategy"
                      value={strategyViewModel.selectedPath}
                      disabled={
                        strategyViewModel.libraryState === "loading" ||
                        !strategyViewModel.library?.strategies.length
                      }
                      onChange={(event) => {
                        setStrategyViewModel((current) => ({
                          ...current,
                          selectedPath: event.target.value,
                        }));
                      }}
                    >
                      {strategyViewModel.library?.strategies.length ? (
                        strategyViewModel.library.strategies.map((entry) => (
                          <option key={entry.path} value={entry.path}>
                            {entry.name ?? entry.title ?? entry.display_path}
                          </option>
                        ))
                      ) : (
                        <option value="">No strategies available</option>
                      )}
                    </select>
                  </label>
                  <label className="field field--wide">
                    <span>Upload strategy file</span>
                    <input
                      ref={strategyUploadInputRef}
                      aria-label="Upload strategy file"
                      type="file"
                      accept=".md,text/markdown"
                      disabled={pendingAction !== null}
                      onChange={(event) => {
                        setSelectedStrategyUploadFile(event.target.files?.[0] ?? null);
                      }}
                    />
                  </label>
                  <div className="action-row">
                    <button
                      className="command-button"
                      type="button"
                      disabled={!canUploadSelectedStrategyFile}
                      onClick={() => {
                        void uploadSelectedStrategyFile();
                      }}
                    >
                      Upload to library
                    </button>
                    <button
                      className="command-button"
                      type="button"
                      disabled={strategyViewModel.libraryState === "loading"}
                      onClick={() => {
                        void refreshStrategyLibrary();
                      }}
                    >
                      Refresh library
                    </button>
                    <button
                      className="command-button"
                      type="button"
                      disabled={
                        !strategyViewModel.selectedPath ||
                        strategyViewModel.validationState === "loading"
                      }
                      onClick={() => {
                        void refreshStrategyValidation(strategyViewModel.selectedPath);
                      }}
                    >
                      Validate selection
                    </button>
                    <button
                      className="command-button"
                      type="button"
                      disabled={!canLoadSelectedStrategy}
                      onClick={() => {
                        void (async () => {
                          const result = await executeLifecycleCommand(
                            {
                              kind: "load_strategy",
                              path: strategyViewModel.selectedPath,
                            },
                            {
                              pendingLabel: "Loading strategy through runtime host",
                            },
                          );

                          if (result?.httpStatus === 200) {
                            void refreshStrategyValidation(strategyViewModel.selectedPath);
                          }
                        })();
                      }}
                    >
                      Load selected strategy
                    </button>
                  </div>
                </div>
                <div className="pill-row">
                  <Pill
                    label={
                      selectedStrategyEntry
                        ? selectedStrategyEntry.valid
                          ? "Library entry valid"
                          : "Library entry needs fixes"
                        : "No strategy selected"
                    }
                    tone={strategyTone(selectedStrategyEntry)}
                  />
                  <Pill
                    label={
                      strategyViewModel.validation
                        ? strategyViewModel.validation.valid
                          ? "Validation passed"
                          : "Validation failed"
                        : strategyViewModel.validationState === "loading"
                          ? "Validation running"
                          : "Validation idle"
                    }
                    tone={
                      strategyViewModel.validationState === "loading"
                        ? "info"
                        : validationTone(strategyViewModel.validation)
                    }
                  />
                  <Pill
                    label={`${strategyViewModel.validation?.warnings.length ?? 0} warning(s)`}
                    tone={
                      (strategyViewModel.validation?.warnings.length ?? 0) > 0
                        ? "warning"
                        : "healthy"
                    }
                  />
                  <Pill
                    label={`${strategyViewModel.validation?.errors.length ?? 0} error(s)`}
                    tone={
                      (strategyViewModel.validation?.errors.length ?? 0) > 0
                        ? "danger"
                        : "healthy"
                    }
                  />
                </div>
                <dl className="definition-list">
                  <Definition
                    label="Selected"
                    value={strategyLabel(strategyViewModel.validation)}
                  />
                  <Definition
                    label="Path"
                    value={
                      strategyViewModel.validation?.display_path ??
                      selectedStrategyEntry?.display_path ??
                      "No strategy selected"
                    }
                  />
                  <Definition
                    label="Scanned roots"
                    value={
                      strategyViewModel.library?.scanned_roots.length
                        ? strategyViewModel.library.scanned_roots.join(" | ")
                        : "No strategy library roots detected"
                    }
                  />
                  <Definition
                    label="Load status"
                    value={
                      snapshot?.status.current_strategy?.path === strategyViewModel.selectedPath
                        ? "Loaded into runtime"
                        : "Not loaded"
                    }
                  />
                  <Definition
                    label="Upload ready"
                    value={
                      selectedStrategyUploadFile
                        ? selectedStrategyUploadFile.name
                        : "Choose a local Markdown strategy file"
                    }
                  />
                </dl>
                {strategyViewModel.libraryError ? (
                  <p className="control-card__note">{strategyViewModel.libraryError}</p>
                ) : null}
                {strategyViewModel.validationError ? (
                  <p className="control-card__note">{strategyViewModel.validationError}</p>
                ) : null}
                {strategyViewModel.validation?.errors.length ? (
                  <ul className="issue-list">
                    {strategyViewModel.validation.errors.slice(0, 3).map((issue, index) => (
                      <li key={`${issue.message}-${index}`}>
                        {issue.message}
                      </li>
                    ))}
                  </ul>
                ) : null}
                {strategyViewModel.validation?.warnings.length ? (
                  <ul className="issue-list issue-list--warning">
                    {strategyViewModel.validation.warnings.slice(0, 3).map((issue, index) => (
                      <li key={`${issue.message}-${index}`}>
                        {issue.message}
                      </li>
                    ))}
                  </ul>
                ) : null}
                <p className="control-card__note">
                  The dashboard now uploads, browses, validates, and loads strategy Markdown only
                  through the local runtime host, keeping file writes and validation inside the
                  backend-owned strategy library workflow.
                </p>
              </section>

              <section className="control-card control-card--wide">
                <p className="control-card__title">Runtime settings</p>
                <div className="pill-row">
                  <Pill
                    label={
                      snapshot.settings.persistence_mode === "config_file"
                        ? "Config file backed"
                        : "Session only"
                    }
                    tone={
                      snapshot.settings.persistence_mode === "config_file" ? "healthy" : "warning"
                    }
                  />
                  <Pill
                    label={snapshot.settings.restart_required ? "Restart required" : "Live applied"}
                    tone={snapshot.settings.restart_required ? "warning" : "healthy"}
                  />
                  <Pill
                    label={snapshot.settings.config_file_path ?? "No config file path"}
                    tone={snapshot.settings.config_file_path ? "info" : "warning"}
                  />
                </div>
                <div className="control-grid">
                  <label className="field">
                    <span>Startup mode</span>
                    <select
                      aria-label="Runtime startup mode"
                      value={settingsDraft?.startupMode ?? snapshot.settings.editable.startup_mode}
                      disabled={pendingAction !== null}
                      onChange={(event) => {
                        updateSettingsDraft((current) => ({
                          ...current,
                          startupMode: event.target.value as RuntimeMode,
                        }));
                      }}
                    >
                      <option value="paper">Paper</option>
                      <option value="observation">Observation</option>
                      <option value="paused">Paused</option>
                      <option value="live">Live</option>
                    </select>
                  </label>
                  <label className="field field--wide">
                    <span>Default strategy path</span>
                    <input
                      aria-label="Default strategy path"
                      placeholder="strategies/examples/gc_momentum_fade_v1.md"
                      value={
                        settingsDraft?.defaultStrategyPath ??
                        (snapshot.settings.editable.default_strategy_path ?? "")
                      }
                      disabled={pendingAction !== null}
                      onChange={(event) => {
                        updateSettingsDraft((current) => ({
                          ...current,
                          defaultStrategyPath: event.target.value,
                        }));
                      }}
                    />
                  </label>
                  <label className="field">
                    <span>Persistence fallback</span>
                    <select
                      aria-label="Persistence fallback policy"
                      value={
                        (settingsDraft?.allowSqliteFallback ??
                        snapshot.settings.editable.allow_sqlite_fallback)
                          ? "allow"
                          : "block"
                      }
                      disabled={pendingAction !== null}
                      onChange={(event) => {
                        updateSettingsDraft((current) => ({
                          ...current,
                          allowSqliteFallback: event.target.value === "allow",
                        }));
                      }}
                    >
                      <option value="block">Require primary Postgres</option>
                      <option value="allow">Allow SQLite fallback</option>
                    </select>
                  </label>
                  <label className="field">
                    <span>Paper account name</span>
                    <input
                      aria-label="Paper account name"
                      placeholder="paper-primary"
                      value={
                        settingsDraft?.paperAccountName ??
                        (snapshot.settings.editable.paper_account_name ?? "")
                      }
                      disabled={pendingAction !== null}
                      onChange={(event) => {
                        updateSettingsDraft((current) => ({
                          ...current,
                          paperAccountName: event.target.value,
                        }));
                      }}
                    />
                  </label>
                  <label className="field">
                    <span>Live account name</span>
                    <input
                      aria-label="Live account name"
                      placeholder="live-primary"
                      value={
                        settingsDraft?.liveAccountName ??
                        (snapshot.settings.editable.live_account_name ?? "")
                      }
                      disabled={pendingAction !== null}
                      onChange={(event) => {
                        updateSettingsDraft((current) => ({
                          ...current,
                          liveAccountName: event.target.value,
                        }));
                      }}
                    />
                  </label>
                </div>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={!canSaveSettings}
                    onClick={() => {
                      void saveRuntimeSettings();
                    }}
                  >
                    Save runtime settings
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={!settingsDirty || pendingAction !== null}
                    onClick={() => {
                      const nextDraft = settingsDraftFromSnapshot(snapshot.settings);
                      settingsDraftRef.current = nextDraft;
                      setSettingsDraft(nextDraft);
                      setSettingsDirty(false);
                    }}
                  >
                    Reset form
                  </button>
                </div>
                <dl className="definition-list">
                  <Definition label="HTTP bind" value={snapshot.settings.http_bind} />
                  <Definition label="WebSocket bind" value={snapshot.settings.websocket_bind} />
                  <Definition
                    label="Config path"
                    value={snapshot.settings.config_file_path ?? "Runtime launched without a config file"}
                  />
                  <Definition
                    label="Effective path"
                    value={
                      snapshot.settings.editable.default_strategy_path ??
                      "No default strategy path"
                    }
                  />
                </dl>
                <p className="control-card__note">{snapshot.settings.detail}</p>
              </section>

              <section className="control-card">
                <p className="control-card__title">Warmup</p>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null || !snapshot.status.strategy_loaded}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "start_warmup" },
                        { pendingLabel: "Starting warmup" },
                      );
                    }}
                  >
                    Start warmup
                  </button>
                </div>
                <p className="control-card__note">
                  Strategy loaded: {snapshot.status.strategy_loaded ? "Yes" : "No"} | Warmup:{" "}
                  {formatMode(snapshot.status.warmup_status)}
                </p>
              </section>

              <section className="control-card">
                <p className="control-card__title">Arming</p>
                <div className="action-row">
                  <button
                    className={
                      snapshot.status.arm_state === "armed"
                        ? "command-button"
                        : "command-button command-button--danger"
                    }
                    type="button"
                    disabled={pendingAction !== null}
                    onClick={() => {
                      if (snapshot.status.arm_state === "armed") {
                        void executeLifecycleCommand(
                          { kind: "disarm" },
                          { pendingLabel: "Disarming runtime" },
                        );
                        return;
                      }

                      const allowOverride = snapshot.readiness.report.hard_override_required;
                      const confirmMessage = allowOverride
                        ? "Arm now with a temporary hard override for this session?"
                        : snapshot.status.mode === "live"
                          ? "Arm LIVE trading? This enables live execution once commands or strategy logic fire."
                          : "Arm the runtime for paper or observation execution?";

                      void executeLifecycleCommand(
                        { kind: "arm", allow_override: allowOverride },
                        {
                          pendingLabel: allowOverride
                            ? "Arming runtime with temporary override"
                            : "Arming runtime",
                          confirmMessage,
                        },
                      );
                    }}
                  >
                    {armButtonLabel}
                  </button>
                </div>
                <p className="control-card__note">
                  Arm state: {formatMode(snapshot.status.arm_state)} | Override required:{" "}
                  {snapshot.readiness.report.hard_override_required ? "Yes" : "No"}
                </p>
              </section>

              <section className="control-card">
                <p className="control-card__title">Flow Control</p>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: snapshot.status.mode === "paused" ? "resume" : "pause" },
                        {
                          pendingLabel:
                            snapshot.status.mode === "paused"
                              ? "Resuming runtime"
                              : "Pausing runtime",
                        },
                      );
                    }}
                  >
                    {pauseButtonLabel}
                  </button>
                </div>
                <p className="control-card__note">
                  Use pause to stop new entries without changing the selected trading mode.
                </p>
              </section>

              <section className="control-card control-card--wide">
                <p className="control-card__title">Operator actions</p>
                <div className="control-grid">
                  <section className="control-card control-card--wide">
                    <p className="control-card__title">Manual entry</p>
                    <form
                      className="flatten-form"
                      onSubmit={(event) => {
                        event.preventDefault();
                        if (!canManualEntry) {
                          return;
                        }

                        void (async () => {
                          const result = await executeLifecycleCommand(
                            {
                              kind: "manual_entry",
                              side: manualEntrySide,
                              quantity: Number.parseInt(manualEntryQuantity, 10),
                              tick_size: manualEntryTickSize.trim(),
                              entry_reference_price: manualEntryReferencePrice.trim(),
                              tick_value_usd: manualEntryTickValueUsd.trim() || null,
                              reason: manualEntryReason.trim(),
                            },
                            {
                              pendingLabel: `Submitting manual ${manualEntrySide} entry`,
                              confirmMessage:
                                "Submit a manual entry through the loaded strategy and runtime safety path now?",
                            },
                          );

                          if (result?.httpStatus === 200) {
                            setManualEntryReason("dashboard manual entry");
                          }
                        })();
                      }}
                    >
                      <div className="control-grid">
                        <label className="field">
                          <span>Side</span>
                          <select
                            aria-label="Manual entry side"
                            value={manualEntrySide}
                            onChange={(event) => {
                              setManualEntrySide(event.target.value as "buy" | "sell");
                            }}
                          >
                            <option value="buy">Buy</option>
                            <option value="sell">Sell</option>
                          </select>
                        </label>
                        <label className="field">
                          <span>Quantity</span>
                          <input
                            aria-label="Manual entry quantity"
                            inputMode="numeric"
                            value={manualEntryQuantity}
                            onChange={(event) => {
                              setManualEntryQuantity(event.target.value);
                            }}
                          />
                        </label>
                        <label className="field">
                          <span>Tick size</span>
                          <input
                            aria-label="Manual entry tick size"
                            inputMode="decimal"
                            placeholder="0.25"
                            value={manualEntryTickSize}
                            onChange={(event) => {
                              setManualEntryTickSize(event.target.value);
                            }}
                          />
                        </label>
                        <label className="field">
                          <span>Reference price</span>
                          <input
                            aria-label="Manual entry reference price"
                            inputMode="decimal"
                            placeholder="2410.50"
                            value={manualEntryReferencePrice}
                            onChange={(event) => {
                              setManualEntryReferencePrice(event.target.value);
                            }}
                          />
                        </label>
                        <label className="field">
                          <span>Tick value USD</span>
                          <input
                            aria-label="Manual entry tick value"
                            inputMode="decimal"
                            placeholder="Optional for risk-based sizing"
                            value={manualEntryTickValueUsd}
                            onChange={(event) => {
                              setManualEntryTickValueUsd(event.target.value);
                            }}
                          />
                        </label>
                      </div>
                      <label className="field field--wide">
                        <span>Reason</span>
                        <input
                          aria-label="Manual entry reason"
                          placeholder="dashboard manual entry"
                          value={manualEntryReason}
                          onChange={(event) => {
                            setManualEntryReason(event.target.value);
                          }}
                        />
                      </label>
                      <button
                        className="command-button"
                        type="submit"
                        disabled={pendingAction !== null || !canManualEntry}
                      >
                        Submit manual entry
                      </button>
                    </form>
                    <p className="control-card__note">
                      Manual entry reuses the loaded strategy for order type, reversal, and
                      broker-protection handling. Reference price and tick inputs keep the
                      execution path explicit and strategy-agnostic.
                    </p>
                  </section>

                  <section className="control-card control-card--wide">
                    <p className="control-card__title">Flatten current position</p>
                    <form
                      className="flatten-form"
                      onSubmit={(event) => {
                        event.preventDefault();
                        if (!canClosePosition) {
                          return;
                        }

                        void (async () => {
                          const result = await executeLifecycleCommand(
                            {
                              kind: "close_position",
                              contract_id: null,
                              reason: closePositionReason.trim(),
                            },
                            {
                              pendingLabel: "Flattening active broker position",
                              confirmMessage:
                                "Flatten the active broker position now? The runtime host will resolve the current contract from the synchronized broker snapshot and dispatch the audited flatten path.",
                            },
                          );

                          if (result?.httpStatus === 200) {
                            setClosePositionReason("dashboard flatten position request");
                          }
                        })();
                      }}
                    >
                      <label className="field field--wide">
                        <span>Reason</span>
                        <input
                          aria-label="Flatten position reason"
                          placeholder="dashboard flatten position request"
                          value={closePositionReason}
                          onChange={(event) => {
                            setClosePositionReason(event.target.value);
                          }}
                        />
                      </label>
                      <button
                        className="command-button command-button--danger"
                        type="submit"
                        disabled={pendingAction !== null || !canClosePosition}
                      >
                        Flatten current position
                      </button>
                    </form>
                    <p className="control-card__note">
                      This is the direct dashboard flatten control. The runtime host resolves the
                      active broker contract from the synchronized snapshot and keeps the action on
                      the same audited close/flatten path used elsewhere.
                    </p>
                  </section>

                  <section className="control-card control-card--wide">
                    <p className="control-card__title">Cancel working orders</p>
                    <form
                      className="flatten-form"
                      onSubmit={(event) => {
                        event.preventDefault();
                        if (!canCancelWorkingOrders) {
                          return;
                        }

                        void (async () => {
                          const result = await executeLifecycleCommand(
                            {
                              kind: "cancel_working_orders",
                              reason: cancelWorkingOrdersReason.trim(),
                            },
                            {
                              pendingLabel: "Cancelling working broker orders",
                              confirmMessage:
                                "Cancel all working broker orders for the loaded market now?",
                            },
                          );

                          if (result?.httpStatus === 200) {
                            setCancelWorkingOrdersReason(
                              "dashboard cancel working orders request",
                            );
                          }
                        })();
                      }}
                    >
                      <label className="field field--wide">
                        <span>Reason</span>
                        <input
                          aria-label="Cancel working orders reason"
                          placeholder="dashboard cancel working orders request"
                          value={cancelWorkingOrdersReason}
                          onChange={(event) => {
                            setCancelWorkingOrdersReason(event.target.value);
                          }}
                        />
                      </label>
                      <button
                        className="command-button"
                        type="submit"
                        disabled={pendingAction !== null || !canCancelWorkingOrders}
                      >
                        Cancel working orders
                      </button>
                    </form>
                  </section>
                </div>
                <p className="control-card__note">
                  All three actions stay inside the local runtime host. Manual entry uses the
                  loaded strategy and synchronized market contract, close resolves the active
                  contract automatically when there is a single live position, and cancel routes
                  only the current market&apos;s working orders through the audited backend path.
                </p>
              </section>
            </div>
          </Panel>

          <Panel
            eyebrow="Runtime"
            title={reviewSummary(snapshot.status)}
            detail={`HTTP ${snapshot.status.http_bind} | WS ${snapshot.status.websocket_bind}`}
          >
            <div className="metric-row">
              <Metric label="Arm state" value={formatMode(snapshot.status.arm_state)} />
              <Metric label="Warmup" value={formatMode(snapshot.status.warmup_status)} />
              <Metric
                label="Account"
                value={snapshot.status.current_account_name ?? "Not selected"}
              />
              <Metric
                label="Dispatch"
                value={snapshot.status.command_dispatch_ready ? "Ready" : "Blocked"}
              />
            </div>
            <div className="pill-row">
              <Pill label={formatMode(snapshot.status.mode)} tone={statusTone("info")} />
              <Pill
                label={snapshot.status.strategy_loaded ? "Strategy loaded" : "No strategy"}
                tone={snapshot.status.strategy_loaded ? "healthy" : "warning"}
              />
              <Pill
                label={
                  snapshot.status.hard_override_active
                    ? "Temporary override active"
                    : "No override"
                }
                tone={snapshot.status.hard_override_active ? "warning" : "healthy"}
              />
              <Pill
                label={
                  snapshot.status.operator_new_entries_enabled
                    ? "Entry gate open"
                    : "Entry gate closed"
                }
                tone={
                  snapshot.status.operator_new_entries_enabled ? "healthy" : "warning"
                }
              />
              <Pill
                label={snapshot.status.command_dispatch_detail}
                tone={snapshot.status.command_dispatch_ready ? "healthy" : "warning"}
              />
            </div>
            <dl className="definition-list">
              <Definition
                label="Strategy"
                value={
                  snapshot.status.current_strategy
                    ? `${snapshot.status.current_strategy.name} v${snapshot.status.current_strategy.version}`
                    : "Not loaded"
                }
              />
              <Definition
                label="New entries"
                value={
                  snapshot.status.operator_new_entries_enabled
                    ? "Enabled"
                    : snapshot.status.operator_new_entries_reason ??
                      "Disabled by operator control"
                }
              />
              <Definition
                label="Market"
                value={
                  snapshot.status.instrument_mapping?.summary ??
                  snapshot.status.instrument_resolution_error ??
                  "Instrument mapping unavailable"
                }
              />
              <Definition
                label="Broker route"
                value={
                  snapshot.status.broker_status?.selected_account
                    ? `${snapshot.status.broker_status.selected_account.account_name} (${snapshot.status.broker_status.selected_account.routing})`
                    : "Account routing unavailable"
                }
              />
            </dl>
          </Panel>

          <Panel
            eyebrow="Readiness"
            title="Grouped pre-arm checks"
            detail={formatDateTime(snapshot.readiness.report.generated_at)}
          >
            <div className="metric-row">
              <Metric label="Pass" value={formatInteger(readinessCounts.pass)} />
              <Metric label="Warning" value={formatInteger(readinessCounts.warning)} />
              <Metric label="Blocking" value={formatInteger(readinessCounts.blocking)} />
              <Metric
                label="Override required"
                value={snapshot.readiness.report.hard_override_required ? "Yes" : "No"}
              />
            </div>
            <ul className="checklist">
              {snapshot.readiness.report.checks.map((check) => (
                <li key={check.name} className="checklist__item">
                  <div className="checklist__header">
                    <strong>{check.name}</strong>
                    <Pill label={formatMode(check.status)} tone={statusTone(check.status)} />
                  </div>
                  <p>{check.message}</p>
                </li>
              ))}
            </ul>
            <p className="panel__footnote">{snapshot.readiness.report.risk_summary}</p>
          </Panel>

          <Panel eyebrow="Health" title="Broker, feed, storage, and host telemetry">
            <div className="metric-row">
              <Metric label="Host" value={formatMode(snapshot.health.status)} />
              <Metric
                label="Broker"
                value={
                  snapshot.status.broker_status
                    ? formatMode(snapshot.status.broker_status.health)
                    : "Unavailable"
                }
              />
              <Metric
                label="Feed"
                value={
                  snapshot.status.market_data_status
                    ? formatMode(snapshot.status.market_data_status.session.market_data.health)
                    : "Unavailable"
                }
              />
              <Metric
                label="Errors"
                value={formatInteger(snapshot.health.system_health?.error_count)}
              />
            </div>
            <dl className="definition-list">
              <Definition
                label="Broker sync"
                value={
                  snapshot.status.broker_status
                    ? formatMode(snapshot.status.broker_status.sync_state)
                    : "Unavailable"
                }
              />
              <Definition
                label="Feed detail"
                value={snapshot.status.market_data_detail ?? "No degraded feed detail"}
              />
              <Definition
                label="Storage"
                value={`${snapshot.status.storage_status.active_backend} | ${snapshot.status.storage_status.detail}`}
              />
              <Definition
                label="Journal"
                value={`${snapshot.status.journal_status.backend} | ${snapshot.status.journal_status.detail}`}
              />
              <Definition
                label="Warmup"
                value={
                  snapshot.status.market_data_status
                    ? `${formatMode(snapshot.status.market_data_status.session.market_data.warmup.status)} | trade ready ${
                        snapshot.status.market_data_status.trade_ready ? "yes" : "no"
                      }`
                    : "Unavailable"
                }
              />
              <Definition
                label="Dispatch"
                value={snapshot.status.command_dispatch_detail}
              />
            </dl>
            <div className="subgrid">
              <MiniMetric
                label="CPU"
                value={
                  snapshot.health.system_health?.cpu_percent != null
                    ? `${snapshot.health.system_health.cpu_percent.toFixed(1)}%`
                    : "Unavailable"
                }
              />
              <MiniMetric
                label="Memory"
                value={humanMemory(snapshot.health.system_health?.memory_bytes)}
              />
              <MiniMetric
                label="DB write"
                value={formatLatency(snapshot.health.system_health?.db_write_latency_ms)}
              />
              <MiniMetric
                label="Queue lag"
                value={formatLatency(snapshot.health.system_health?.queue_lag_ms)}
              />
              <MiniMetric
                label="Reconnects"
                value={formatInteger(snapshot.health.system_health?.reconnect_count)}
              />
              <MiniMetric
                label="Broker heartbeat"
                value={formatDateTime(snapshot.status.broker_status?.last_heartbeat_at)}
              />
              <MiniMetric
                label="Feed heartbeat"
                value={formatDateTime(
                  snapshot.status.market_data_status?.session.market_data.last_heartbeat_at,
                )}
              />
              <MiniMetric
                label="Last sync"
                value={formatDateTime(snapshot.status.broker_status?.last_sync_at)}
              />
            </div>
            <div className="subgrid subgrid--wide">
              <section className="review-card">
                <p className="control-card__title">Connectivity clocks</p>
                <dl className="definition-list">
                  <Definition
                    label="Broker auth"
                    value={formatDateTime(snapshot.status.broker_status?.last_authenticated_at)}
                  />
                  <Definition
                    label="Broker heartbeat"
                    value={formatDateTime(snapshot.status.broker_status?.last_heartbeat_at)}
                  />
                  <Definition
                    label="Broker sync"
                    value={formatDateTime(snapshot.status.broker_status?.last_sync_at)}
                  />
                  <Definition
                    label="Feed heartbeat"
                    value={formatDateTime(
                      snapshot.status.market_data_status?.session.market_data.last_heartbeat_at,
                    )}
                  />
                  <Definition
                    label="Broker disconnect"
                    value={
                      snapshot.status.broker_status?.last_disconnect_reason ?? "No disconnect reason"
                    }
                  />
                  <Definition
                    label="Feed disconnect"
                    value={
                      snapshot.status.market_data_status?.session.market_data.last_disconnect_reason ??
                      "No disconnect reason"
                    }
                  />
                </dl>
              </section>
              <section className="review-card">
                <p className="control-card__title">Feed and storage detail</p>
                <dl className="definition-list">
                  <Definition
                    label="Replay"
                    value={
                      snapshot.status.market_data_status?.replay_caught_up ? "Caught up" : "Behind"
                    }
                  />
                  <Definition
                    label="Warmup mode"
                    value={formatWarmupMode(snapshot.status.market_data_status?.warmup_mode)}
                  />
                  <Definition
                    label="Primary DB"
                    value={snapshot.status.storage_status.primary_configured ? "Configured" : "Missing"}
                  />
                  <Definition
                    label="Fallback"
                    value={
                      snapshot.status.storage_status.fallback_activated
                        ? "SQLite fallback active"
                        : "Primary backend active"
                    }
                  />
                </dl>
                {feedStatuses.length ? (
                  <ul className="event-list event-list--compact">
                    {feedStatuses.map((feed) => (
                      <li key={`${feed.instrument_symbol}-${feed.feed}`} className="event-list__item">
                        <div className="event-list__header">
                          <strong>{`${feed.instrument_symbol} | ${feed.feed}`}</strong>
                          <Pill label={formatMode(feed.state)} tone="info" />
                        </div>
                        <p>{`${feed.detail} | last update ${formatDateTime(feed.last_event_at)}`}</p>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="panel__footnote">
                    No feed-level status records are projected through the runtime host yet.
                  </p>
                )}
              </section>
            </div>
          </Panel>

          <Panel eyebrow="History" title="Trade state and PnL projection">
            <div className="metric-row">
              <Metric
                label="Open positions"
                value={formatInteger(snapshot.history.projection.open_position_symbols.length)}
              />
              <Metric
                label="Working orders"
                value={formatInteger(snapshot.history.projection.working_order_ids.length)}
              />
              <Metric
                label="Closed trades"
                value={formatInteger(snapshot.history.projection.closed_trade_count)}
              />
              <Metric
                label="Open trades"
                value={formatInteger(snapshot.history.projection.open_trade_ids.length)}
              />
            </div>
            <div className="subgrid subgrid--wide">
              <MiniMetric
                label="Gross PnL"
                value={formatSignedCurrency(snapshot.history.projection.closed_trade_gross_pnl)}
              />
              <MiniMetric
                label="Net PnL"
                value={formatSignedCurrency(snapshot.history.projection.closed_trade_net_pnl)}
              />
              <MiniMetric
                label="Fees"
                value={formatCurrency(snapshot.history.projection.closed_trade_fees)}
              />
              <MiniMetric
                label="Commissions"
                value={formatCurrency(snapshot.history.projection.closed_trade_commissions)}
              />
              <MiniMetric
                label="Slippage"
                value={formatCurrency(snapshot.history.projection.closed_trade_slippage)}
              />
              <MiniMetric
                label="Last activity"
                value={formatDateTime(snapshot.history.projection.last_activity_at)}
              />
            </div>
            <div className="metric-row">
              <Metric
                label="Win rate"
                value={formatPercentage(tradePerformance?.winRate)}
              />
              <Metric
                label="Avg net/trade"
                value={formatSignedCurrency(tradePerformance?.averageNet)}
              />
              <Metric
                label="Avg hold"
                value={formatDurationMinutes(tradePerformance?.averageHoldMinutes)}
              />
              <Metric
                label="Floating net"
                value={formatSignedCurrency(tradePerformance?.floatingNet)}
              />
            </div>
            <dl className="definition-list">
              <Definition
                label="Latest position"
                value={
                  snapshot.history.projection.latest_position
                    ? `${snapshot.history.projection.latest_position.symbol} | ${snapshot.history.projection.latest_position.quantity} @ ${formatDecimal(snapshot.history.projection.latest_position.average_price)}`
                    : "No position record"
                }
              />
              <Definition
                label="Latest PnL snapshot"
                value={
                  snapshot.history.projection.latest_pnl_snapshot
                    ? `${formatSignedCurrency(snapshot.history.projection.latest_pnl_snapshot.net_pnl)} net at ${formatDateTime(snapshot.history.projection.latest_pnl_snapshot.captured_at)}`
                    : "No PnL snapshot"
                }
              />
              <Definition
                label="Latest trade"
                value={
                  snapshot.history.projection.latest_trade_summary
                    ? `${snapshot.history.projection.latest_trade_summary.symbol} | ${formatMode(snapshot.history.projection.latest_trade_summary.status)} | ${formatSignedCurrency(snapshot.history.projection.latest_trade_summary.net_pnl)}`
                    : "No trade summary"
                }
              />
            </dl>
            <section className="review-card review-card--wide">
              <p className="control-card__title">Real-time P&amp;L chart</p>
              {pnlChart && pnlChart.points.length ? (
                <div className="pnl-chart">
                  <div className="pnl-chart__canvas-wrap">
                    <svg
                      className="pnl-chart__canvas"
                      viewBox="0 0 100 100"
                      preserveAspectRatio="none"
                      role="img"
                      aria-label="Real-time P&L chart"
                    >
                      <defs>
                        <linearGradient id="pnl-chart-line" x1="0" x2="1" y1="0" y2="0">
                          <stop offset="0%" stopColor="#0d4d78" />
                          <stop offset="55%" stopColor="#0f6694" />
                          <stop offset="100%" stopColor="#ef8a2b" />
                        </linearGradient>
                      </defs>
                      {pnlChart.zeroPercent !== null ? (
                        <line
                          className="pnl-chart__baseline"
                          x1="4"
                          x2="96"
                          y1={pnlChart.zeroPercent}
                          y2={pnlChart.zeroPercent}
                        />
                      ) : null}
                      <path className="pnl-chart__line" d={pnlChartPathData} />
                      {pnlChart.points.map((point) => (
                        <circle
                          key={point.id}
                          className={`pnl-chart__dot pnl-chart__dot--${point.tone}`}
                          cx={point.xPercent}
                          cy={point.yPercent}
                          r="2.6"
                        />
                      ))}
                    </svg>
                  </div>
                  <div className="pnl-chart__points">
                    {pnlChart.points.map((point) => (
                      <article key={point.id} className="pnl-chart__point-card">
                        <div className="pnl-chart__point-header">
                          <span className={`pnl-chart__point-pill pnl-chart__point-pill--${point.tone}`}>
                            {point.label}
                          </span>
                          <strong>{formatSignedCurrency(point.value)}</strong>
                        </div>
                        <span className="pnl-chart__point-note">{point.note}</span>
                      </article>
                    ))}
                  </div>
                </div>
              ) : (
                <p className="panel__footnote">
                  The runtime host has not projected enough history to draw the real-time P&amp;L chart yet.
                </p>
              )}
              <div className="subgrid">
                <MiniMetric
                  label="Floating now"
                  value={formatSignedCurrency(tradePerformance?.floatingNet)}
                />
                <MiniMetric
                  label="Average closed net"
                  value={formatSignedCurrency(tradePerformance?.averageNet)}
                />
                <MiniMetric
                  label="Win rate"
                  value={formatPercentage(tradePerformance?.winRate)}
                />
                <MiniMetric
                  label="Average hold"
                  value={formatDurationMinutes(tradePerformance?.averageHoldMinutes)}
                />
                <MiniMetric
                  label="Largest win"
                  value={formatSignedCurrency(tradePerformance?.largestWin)}
                />
                <MiniMetric
                  label="Largest loss"
                  value={formatSignedCurrency(tradePerformance?.largestLoss)}
                />
                <MiniMetric
                  label="Tracked closed trades"
                  value={formatInteger(tradePerformance?.closedCount)}
                />
                <MiniMetric
                  label="Tracked open trades"
                  value={formatInteger(tradePerformance?.openCount)}
                />
              </div>
              <p className="control-card__note">
                This chart is derived from the projected trade summaries plus the latest persisted floating P&amp;L snapshot that come through the local `/history` endpoint.
              </p>
            </section>
            <section className="review-card review-card--wide">
              <p className="control-card__title">Per-trade P&amp;L</p>
              {perTradePnl.length ? (
                <div className="per-trade-pnl-grid">
                  {perTradePnl.map((trade) => (
                    <article key={trade.tradeId} className="per-trade-pnl-card">
                      <div className="event-list__header">
                        <strong>{`${trade.symbol} | ${formatMode(trade.side)} ${formatInteger(trade.quantity)}`}</strong>
                        <Pill label={formatSignedCurrency(trade.netPnl)} tone={trade.tone} />
                      </div>
                      <p className="event-list__meta">
                        {`Trade ${trade.tradeId} | ${formatMode(trade.status)} | opened ${formatDateTime(
                          trade.openedAt,
                        )}${trade.closedAt ? ` | closed ${formatDateTime(trade.closedAt)}` : ""} | hold ${formatDurationMinutes(
                          trade.holdMinutes,
                        )}`}
                      </p>
                      <div className="mini-metric-grid">
                        <MiniMetric label="Gross" value={formatSignedCurrency(trade.grossPnl)} />
                        <MiniMetric label="Net" value={formatSignedCurrency(trade.netPnl)} />
                        <MiniMetric label="Fees" value={formatCurrency(trade.fees)} />
                        <MiniMetric
                          label="Commissions"
                          value={formatCurrency(trade.commissions)}
                        />
                        <MiniMetric label="Slippage" value={formatCurrency(trade.slippage)} />
                      </div>
                    </article>
                  ))}
                </div>
              ) : (
                <p className="panel__footnote">
                  No trade summaries are projected yet, so per-trade P&amp;L is unavailable.
                </p>
              )}
              <p className="control-card__note">
                Each per-trade card is rendered from the host-projected trade summary instead of frontend-calculated outcomes.
              </p>
            </section>
            <div className="subgrid">
              <section className="review-card">
                <p className="control-card__title">Open working orders</p>
                {openWorkingOrders.length ? (
                  <ul className="event-list">
                    {openWorkingOrders.map((order) => (
                      <li key={order.broker_order_id} className="event-list__item">
                        <div className="event-list__header">
                          <strong>{`${order.symbol} | ${formatMode(order.side)} ${formatInteger(order.quantity)}`}</strong>
                          <Pill label={formatMode(order.status)} tone="warning" />
                        </div>
                        <p>
                          {`Order ${order.broker_order_id} | ${order.order_type ?? "unknown"} | filled ${formatInteger(order.filled_quantity)} | updated ${formatDateTime(order.updated_at)}`}
                        </p>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="panel__footnote">No working broker orders are currently projected.</p>
                )}
              </section>
              <section className="review-card">
                <p className="control-card__title">Recent fills</p>
                {recentFills.length ? (
                  <ul className="event-list">
                    {recentFills.map((fill) => (
                      <li key={fill.fill_id} className="event-list__item">
                        <div className="event-list__header">
                          <strong>{`${fill.symbol} | ${formatMode(fill.side)} ${formatInteger(fill.quantity)}`}</strong>
                          <Pill label={formatDecimal(fill.price)} tone="info" />
                        </div>
                        <p>
                          {`Fill ${fill.fill_id}${fill.broker_order_id ? ` | order ${fill.broker_order_id}` : ""} | fees ${formatCurrency(fill.fee)} | commissions ${formatCurrency(fill.commission)} | ${formatDateTime(fill.occurred_at)}`}
                        </p>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="panel__footnote">No broker fills have been recorded yet.</p>
                )}
              </section>
              <section className="review-card">
                <p className="control-card__title">Trade ledger</p>
                {recentTrades.length ? (
                  <ul className="event-list">
                    {recentTrades.map((trade) => (
                      <li key={trade.trade_id} className="event-list__item">
                        <div className="event-list__header">
                          <strong>{`${trade.symbol} | ${formatMode(trade.side)} ${formatInteger(trade.quantity)}`}</strong>
                          <Pill label={formatMode(trade.status)} tone={tradeTone(trade)} />
                        </div>
                        <p className="event-list__meta">
                          {`Trade ${trade.trade_id} | opened ${formatDateTime(trade.opened_at)}${
                            trade.closed_at ? ` | closed ${formatDateTime(trade.closed_at)}` : ""
                          } | hold ${formatDurationMinutes(
                            minutesBetween(trade.opened_at, trade.closed_at),
                          )}`}
                        </p>
                        <p>
                          {`Entry ${formatDecimal(trade.average_entry_price)}${
                            trade.average_exit_price
                              ? ` | exit ${formatDecimal(trade.average_exit_price)}`
                              : ""
                          } | gross ${formatSignedCurrency(trade.gross_pnl)} | net ${formatSignedCurrency(
                            trade.net_pnl,
                          )} | fees ${formatCurrency(trade.fees)} | commissions ${formatCurrency(
                            trade.commissions,
                          )} | slippage ${formatCurrency(trade.slippage)}`}
                        </p>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="panel__footnote">
                    No trade summaries are projected yet.
                  </p>
                )}
              </section>
            </div>
            {projectedPnlSnapshot ? (
              <p className="panel__footnote">
                Latest floating snapshot: {formatSignedCurrency(projectedPnlSnapshot.net_pnl)} net,{" "}
                {formatSignedCurrency(projectedPnlSnapshot.unrealized_pnl)} unrealized, captured{" "}
                {formatDateTime(projectedPnlSnapshot.captured_at)}.
              </p>
            ) : null}
          </Panel>

          <Panel eyebrow="Latency" title="Latest trade-path timing">
            <div className="metric-row">
              <Metric
                label="Recorded paths"
                value={formatInteger(snapshot.status.recorded_trade_latency_count)}
              />
              <Metric
                label="End to end fill"
                value={formatLatency(latestLatency(snapshot.status))}
              />
              <Metric
                label="Broker ack"
                value={formatLatency(snapshot.health.latest_trade_latency?.latency.broker_ack_latency_ms)}
              />
              <Metric
                label="Sync update"
                value={formatLatency(snapshot.health.latest_trade_latency?.latency.sync_update_latency_ms)}
              />
            </div>
            <dl className="definition-list">
              <Definition
                label="Latest record"
                value={
                  snapshot.health.latest_trade_latency
                    ? formatDateTime(snapshot.health.latest_trade_latency.recorded_at)
                    : "No trade-path record yet"
                }
              />
              <Definition
                label="Strategy"
                value={snapshot.health.latest_trade_latency?.strategy_id ?? "Unavailable"}
              />
              <Definition
                label="Action"
                value={snapshot.health.latest_trade_latency?.action_id ?? "Unavailable"}
              />
              <Definition
                label="Slowest stage"
                value={
                  slowestLatencyStage
                    ? `${slowestLatencyStage.label} | ${formatLatency(slowestLatencyStage.value)}`
                    : "No latency record yet"
                }
              />
            </dl>
            <div className="subgrid subgrid--wide">
              <section className="review-card">
                <p className="control-card__title">Latency stage breakdown</p>
                {latencyBreakdown.some((stage) => stage.value !== null) ? (
                  <ul className="latency-list">
                    {latencyBreakdown.map((stage) => (
                      <li key={stage.key} className="latency-list__item">
                        <div className="latency-list__header">
                          <strong>{stage.label}</strong>
                          <span>{formatLatency(stage.value)}</span>
                        </div>
                        <div className="latency-list__track">
                          <span
                            className="latency-list__bar"
                            style={{ width: `${stage.barPercent}%` }}
                          />
                        </div>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="panel__footnote">
                    The runtime has not published a trade-path latency record yet.
                  </p>
                )}
              </section>
              <section className="review-card">
                <p className="control-card__title">Latency and host correlation</p>
                <div className="subgrid">
                  <MiniMetric
                    label="Signal"
                    value={formatLatency(snapshot.health.latest_trade_latency?.latency.signal_latency_ms)}
                  />
                  <MiniMetric
                    label="Decision"
                    value={formatLatency(snapshot.health.latest_trade_latency?.latency.decision_latency_ms)}
                  />
                  <MiniMetric
                    label="Order send"
                    value={formatLatency(snapshot.health.latest_trade_latency?.latency.order_send_latency_ms)}
                  />
                  <MiniMetric
                    label="Fill"
                    value={formatLatency(snapshot.health.latest_trade_latency?.latency.fill_latency_ms)}
                  />
                </div>
                <dl className="definition-list">
                  <Definition
                    label="DB write latency"
                    value={formatLatency(snapshot.health.system_health?.db_write_latency_ms)}
                  />
                  <Definition
                    label="Queue lag"
                    value={formatLatency(snapshot.health.system_health?.queue_lag_ms)}
                  />
                  <Definition
                    label="Reconnect count"
                    value={formatInteger(snapshot.health.system_health?.reconnect_count)}
                  />
                  <Definition
                    label="Latest record"
                    value={
                      snapshot.health.latest_trade_latency
                        ? `${snapshot.health.latest_trade_latency.action_id} at ${formatDateTime(
                            snapshot.health.latest_trade_latency.recorded_at,
                          )}`
                        : "No trade-path record yet"
                    }
                  />
                </dl>
              </section>
            </div>
          </Panel>

          <Panel eyebrow="Safety" title="Reconnect, shutdown, and operator guardrails">
            <div className="pill-row">
              <Pill
                label={
                  snapshot.status.reconnect_review.required
                    ? "Reconnect review active"
                    : "Reconnect clear"
                }
                tone={reviewTone(snapshot.status.reconnect_review)}
              />
              <Pill
                label={
                  snapshot.status.shutdown_review.blocked ||
                  snapshot.status.shutdown_review.awaiting_flatten
                    ? "Shutdown review active"
                    : "Shutdown clear"
                }
                tone={reviewTone(snapshot.status.shutdown_review)}
              />
            </div>
            <dl className="definition-list">
              <Definition
                label="Reconnect review"
                value={
                  snapshot.status.reconnect_review.reason ??
                  (snapshot.status.reconnect_review.last_decision
                    ? `Last decision: ${formatMode(snapshot.status.reconnect_review.last_decision)}`
                    : "No reconnect review pending")
                }
              />
              <Definition
                label="Shutdown review"
                value={
                  snapshot.status.shutdown_review.reason ??
                  (snapshot.status.shutdown_review.decision
                    ? `Last decision: ${formatMode(snapshot.status.shutdown_review.decision)}`
                    : "No shutdown review pending")
                }
              />
              <Definition
                label="Reconnect counts"
                value={formatInteger(
                  snapshot.status.broker_status?.reconnect_count ??
                    snapshot.health.system_health?.reconnect_count,
                )}
              />
            </dl>
            {snapshot.status.reconnect_review.required ? (
              <section className="review-card">
                <p className="control-card__title">Reconnect review actions</p>
                <label className="field field--wide">
                  <span>Reason</span>
                  <input
                    aria-label="Reconnect review reason"
                    placeholder="dashboard reconnect review resolution"
                    value={reconnectReason}
                    onChange={(event) => {
                      setReconnectReason(event.target.value);
                    }}
                  />
                </label>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={reviewActionsDisabled}
                    onClick={() => {
                      void executeReconnectDecision("reattach_bot_management");
                    }}
                  >
                    Reattach bot management
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={reviewActionsDisabled}
                    onClick={() => {
                      void executeReconnectDecision("leave_broker_protected");
                    }}
                  >
                    Leave broker-side
                  </button>
                  <button
                    className="command-button command-button--danger"
                    type="button"
                    disabled={reconnectCloseDisabled}
                    onClick={() => {
                      void executeReconnectDecision("close_position");
                    }}
                  >
                    Close position
                  </button>
                </div>
                <p className="control-card__note">
                  The runtime host resolves the active contract id when there is only one open
                  broker position, so reconnect-close can stay inside the audited control path.
                </p>
              </section>
            ) : null}
            {snapshot.status.shutdown_review.blocked ? (
              <section className="review-card">
                <p className="control-card__title">Shutdown review actions</p>
                <label className="field field--wide">
                  <span>Reason</span>
                  <input
                    aria-label="Shutdown review reason"
                    placeholder="dashboard shutdown review decision"
                    value={shutdownReason}
                    onChange={(event) => {
                      setShutdownReason(event.target.value);
                    }}
                  />
                </label>
                <div className="action-row">
                  <button
                    className="command-button command-button--danger"
                    type="button"
                    disabled={shutdownFlattenDisabled}
                    onClick={() => {
                      void executeShutdownDecision("flatten_first");
                    }}
                  >
                    Flatten first
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={shutdownLeaveDisabled}
                    onClick={() => {
                      void executeShutdownDecision("leave_broker_protected");
                    }}
                  >
                    Leave broker-protected
                  </button>
                </div>
                <p className="control-card__note">
                  Leave-in-place is only enabled when every open position reports broker-side
                  protection through the runtime host snapshot.
                </p>
              </section>
            ) : null}
            <p className="panel__footnote">
              Reconnect hardening now covers startup and reconnect review decisions through the real
              runtime host. The remaining safety work is the broader paper-mode regression sweep and
              final operator polish.
            </p>
          </Panel>

          <Panel
            eyebrow="Journal"
            title="Persisted operator journal and audit trail"
            detail={`${formatInteger(snapshot.journal.total_records)} total record(s)`}
          >
            <div className="metric-row">
              <Metric label="Info" value={formatInteger(journalSummary.infoCount)} />
              <Metric label="Warnings" value={formatInteger(journalSummary.warningCount)} />
              <Metric label="Errors" value={formatInteger(journalSummary.errorCount)} />
              <Metric
                label="Dashboard actions"
                value={formatInteger(journalSummary.dashboardCount)}
              />
            </div>
            {journalSummary.categories.length ? (
              <div className="pill-row">
                {journalSummary.categories.map((entry) => (
                  <Pill
                    key={entry.category}
                    label={`${entry.category} ${formatInteger(entry.count)}`}
                    tone="info"
                  />
                ))}
              </div>
            ) : null}
            {journalRecords.length ? (
              <ul className="event-list">
                {journalRecords.map((record) => (
                  <li key={record.event_id} className="event-list__item">
                    <div className="event-list__header">
                      <strong>{`${record.category}:${record.action}`}</strong>
                      <Pill
                        label={formatDateTime(record.occurred_at)}
                        tone={journalRecordTone(record)}
                      />
                    </div>
                    <p className="event-list__meta">
                      {`Source ${formatMode(record.source)} | Severity ${formatMode(record.severity)}`}
                    </p>
                    <pre className="payload-block">{prettyJson(record.payload)}</pre>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="panel__footnote">
                No persisted journal records are available through the local runtime host yet.
              </p>
            )}
          </Panel>

          <Panel
            eyebrow="Events"
            title="Local operator feed from /events"
            detail={
              eventFeed.lastEventAt
                ? `Last event ${formatDateTime(eventFeed.lastEventAt)}`
                : "Waiting for the local event stream"
            }
          >
            <div className="pill-row">
              <Pill
                label={`Stream ${eventFeed.connectionState}`}
                tone={
                  eventFeed.connectionState === "open"
                    ? "healthy"
                    : eventFeed.connectionState === "connecting"
                      ? "info"
                      : "warning"
                }
              />
              <Pill
                label={`${formatInteger(eventFeed.recentEvents.length)} recent event(s)`}
                tone="info"
              />
            </div>
            {eventFeed.error ? <p className="panel__footnote">{eventFeed.error}</p> : null}
            {eventFeed.recentEvents.length ? (
              <ul className="event-list">
                {eventFeed.recentEvents.map((event) => (
                  <li key={event.id} className="event-list__item">
                    <div className="event-list__header">
                      <strong>{event.headline}</strong>
                      <Pill label={formatDateTime(event.occurredAt)} tone={event.tone} />
                    </div>
                    <p>{event.detail}</p>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="panel__footnote">
                The dashboard is connected to the local runtime event hub and will render journal,
                readiness, command, health, and history updates here.
              </p>
            )}
          </Panel>
        </div>
      ) : null}
    </main>
  );
}

export default App;
