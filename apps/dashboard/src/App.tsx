import {
  startTransition,
  useEffect,
  useEffectEvent,
  useRef,
  useState,
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
  decimalToNumber,
  latencyStages,
  minutesBetween,
  reviewSummary,
  reviewTone,
  tradeTone,
} from "./lib/dashboardPresentation";
import {
  formatDateTime,
  formatInteger,
  formatLatency,
  formatMode,
  formatSignedCurrency,
  formatWarmupMode,
} from "./lib/format";
import {
  EventsPanel,
  HealthPanel,
  HistoryPanel,
  JournalPanel,
  LatencyPanel,
  ReadinessPanel,
  RuntimeSummaryPanel,
} from "./components/dashboardMonitoring";
import {
  ControlCluster,
  Definition,
  Panel,
  Pill,
  SignalTile,
} from "./components/dashboardPrimitives";
import type {
  BannerTone,
  CommandFeedback,
  CommandOptions,
  EventFeedItem,
  EventFeedViewModel,
  HeadlineSummary,
  JournalSummaryViewModel,
  LatencyStageViewModel,
  PerTradePnlViewModel,
  PnlChartPoint,
  PnlChartViewModel,
  RuntimeSettingsDraft,
  StrategySummaryViewModel,
  TradePerformanceViewModel,
  ViewModel,
} from "./dashboardModels";
import type {
  ControlApiEvent,
  EventJournalRecord,
  FillRecord,
  OrderRecord,
  PnlSnapshotRecord,
  RuntimeLifecycleCommand,
  RuntimeLifecycleResponse,
  RuntimeMode,
  RuntimeEditableSettings,
  RuntimeSettingsSnapshot,
  RuntimeStatusSnapshot,
  RuntimeStrategyCatalogEntry,
  RuntimeStrategyLibraryResponse,
  RuntimeStrategyValidationResponse,
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

function feedbackToneFromHttpStatus(httpStatus: number): BannerTone {
  if (httpStatus >= 500) {
    return "danger";
  }

  if (httpStatus === 409 || httpStatus === 428) {
    return "warning";
  }

  return "healthy";
}

function readinessSummary(counts: { pass: number; warning: number; blocking: number }) {
  if (counts.blocking > 0) {
    return `${counts.blocking} blocking`;
  }

  if (counts.warning > 0) {
    return `${counts.warning} warning`;
  }

  return "Ready";
}

function readinessTone(counts: { pass: number; warning: number; blocking: number }): BannerTone {
  if (counts.blocking > 0) {
    return "danger";
  }

  if (counts.warning > 0) {
    return "warning";
  }

  return "healthy";
}

function dispatchTone(status: RuntimeStatusSnapshot): BannerTone {
  if (status.command_dispatch_ready) {
    return "healthy";
  }

  return status.mode === "observation" ? "info" : "warning";
}

function warmupTone(status: RuntimeStatusSnapshot["warmup_status"]): BannerTone {
  switch (status) {
    case "ready":
      return "healthy";
    case "failed":
      return "danger";
    default:
      return "warning";
  }
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

function summarizeRecentEvents(events: EventFeedItem[]): HeadlineSummary[] {
  const summary = new Map<string, HeadlineSummary>();

  for (const event of events) {
    const existing = summary.get(event.headline);
    if (existing) {
      existing.count += 1;
      continue;
    }

    summary.set(event.headline, {
      headline: event.headline,
      count: 1,
      tone: event.tone,
    });
  }

  return [...summary.values()]
    .sort((left, right) => right.count - left.count || left.headline.localeCompare(right.headline))
    .slice(0, 4);
}

function latestPnlSnapshot(snapshot: DashboardSnapshot): PnlSnapshotRecord | null {
  return snapshot.history.projection.latest_pnl_snapshot;
}

function isPositiveNumberInput(value: string): boolean {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0;
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
  const [newEntriesReason, setNewEntriesReason] = useState("operator gate");
  const [closePositionReason, setClosePositionReason] = useState("flatten position");
  const [manualEntrySide, setManualEntrySide] = useState<"buy" | "sell">("buy");
  const [manualEntryQuantity, setManualEntryQuantity] = useState("1");
  const [manualEntryTickSize, setManualEntryTickSize] = useState("0.1");
  const [manualEntryReferencePrice, setManualEntryReferencePrice] = useState("");
  const [manualEntryTickValueUsd, setManualEntryTickValueUsd] = useState("");
  const [manualEntryReason, setManualEntryReason] = useState("manual entry");
  const [cancelWorkingOrdersReason, setCancelWorkingOrdersReason] = useState(
    "cancel working orders",
  );
  const [reconnectReason, setReconnectReason] = useState("resolve reconnect review");
  const [shutdownReason, setShutdownReason] = useState("resolve shutdown review");
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
      setReconnectReason("resolve reconnect review");
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
      setShutdownReason("resolve shutdown review");
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
    ? snapshot.readiness.report.checks.reduce<{
        pass: number;
        warning: number;
        blocking: number;
      }>(
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
  const eventHeadlineSummary = summarizeRecentEvents(eventFeed.recentEvents);
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
  const readinessState = readinessSummary(readinessCounts);
  const activeReviewSummary = snapshot ? reviewSummary(snapshot.status) : "Awaiting runtime";
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
        <div className="hero__content">
          <div className="hero__copy">
            <p className="eyebrow">TV Bot Operator Console</p>
            <h1>Local runtime command center</h1>
            <p className="hero__summary">
              Operate the runtime, watch the live safety posture, and resolve review-required
              states from the local control plane without losing the backend as the source of
              truth.
            </p>
          </div>
          <div className="hero__meta">
            <div className="hero__mode-lockup">
              <span className="hero__mode-label">Current mode</span>
              <strong>{snapshot ? formatMode(snapshot.status.mode) : "Waiting for runtime"}</strong>
              <span className="hero__mode-detail">{activeReviewSummary}</span>
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
                {snapshot
                  ? formatDateTime(snapshot.fetchedAt)
                  : formatDateTime(viewModel.lastAttemptedAt)}
              </p>
            </div>
          </div>
        </div>
        <div className="hero__rail" aria-label="Runtime posture">
          <SignalTile
            label="Arm state"
            value={snapshot ? formatMode(snapshot.status.arm_state) : "Waiting"}
            detail={
              snapshot
                ? snapshot.status.strategy_loaded
                  ? "Strategy is loaded and tracked by the host"
                  : "No strategy is currently loaded"
                : "Polling local runtime host"
            }
            tone={
              snapshot
                ? snapshot.status.arm_state === "armed"
                  ? "healthy"
                  : "neutral"
                : "info"
            }
          />
          <SignalTile
            label="Readiness"
            value={snapshot ? readinessState : "Waiting"}
            detail={
              snapshot
                ? `${readinessCounts.pass} pass | ${readinessCounts.warning} warning`
                : "Waiting for grouped checks"
            }
            tone={snapshot ? readinessTone(readinessCounts) : "info"}
          />
          <SignalTile
            label="Warmup"
            value={snapshot ? formatMode(snapshot.status.warmup_status) : "Waiting"}
            detail={
              snapshot?.status.market_data_status?.warmup_mode
                ? formatWarmupMode(snapshot.status.market_data_status.warmup_mode)
                : "Awaiting market-data state"
            }
            tone={snapshot ? warmupTone(snapshot.status.warmup_status) : "info"}
          />
          <SignalTile
            label="Dispatch"
            value={
              snapshot
                ? snapshot.status.command_dispatch_ready
                  ? "Ready"
                  : "Blocked"
                : "Waiting"
            }
            detail={
              snapshot
                ? snapshot.status.command_dispatch_ready
                  ? snapshot.status.current_account_name ?? "Runtime host is dispatch-ready"
                  : snapshot.status.command_dispatch_detail
                : "Waiting for dispatcher state"
            }
            tone={snapshot ? dispatchTone(snapshot.status) : "info"}
          />
          <SignalTile
            label="Safety review"
            value={
              snapshot
                ? snapshot.status.reconnect_review.required ||
                  snapshot.status.shutdown_review.blocked ||
                  snapshot.status.shutdown_review.awaiting_flatten
                  ? "Attention"
                  : "Clear"
                : "Waiting"
            }
            detail={
              snapshot
                ? activeReviewSummary
                : "Waiting for reconnect and shutdown review state"
            }
            tone={
              snapshot
                ? snapshot.status.reconnect_review.required ||
                  snapshot.status.shutdown_review.blocked ||
                  snapshot.status.shutdown_review.awaiting_flatten
                  ? "warning"
                  : "healthy"
                : "info"
            }
          />
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
            className="panel--full panel--command-center"
            eyebrow="Control Center"
            title="Lifecycle commands through /runtime/commands"
            detail={`Current mode: ${formatMode(snapshot.status.mode)} | Dispatch: ${snapshot.status.command_dispatch_detail}`}
          >
            <div className="control-shell">
              <ControlCluster
                eyebrow="Mode and gating"
                title="Runtime posture and operator entry controls"
                detail="High-frequency controls for mode selection and fresh-entry gating stay grouped together."
              >
                <div className="control-grid">
                  <section className="control-card control-card--span-4">
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

                  <section className="control-card control-card--span-8">
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
                    placeholder="operator gate"
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
                </div>
              </ControlCluster>

              <ControlCluster
                eyebrow="Strategy and settings"
                title="Library workflow and runtime configuration"
                detail="Strategy selection, upload, validation, and settings edits stay backend-owned."
              >
                <div className="control-grid">

                  <section className="control-card control-card--span-7">
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

                  <section className="control-card control-card--span-5">
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
                </div>
              </ControlCluster>

              <ControlCluster
                eyebrow="Execution controls"
                title="Warmup, arming, and manual operator actions"
                detail="Execution-facing controls stay separate from strategy and settings work."
              >
                <div className="control-grid">

                  <section className="control-card control-card--span-4">
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

                  <section className="control-card control-card--span-4">
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

                  <section className="control-card control-card--span-4">
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

                  <section className="control-card control-card--span-12">
                <p className="control-card__title">Operator actions</p>
                <div className="control-grid">
                  <section className="control-card control-card--span-7">
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
                            setManualEntryReason("manual entry");
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
                    placeholder="manual entry"
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

                  <section className="control-card control-card--span-5">
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
                            setClosePositionReason("flatten position");
                          }
                        })();
                      }}
                    >
                      <label className="field field--wide">
                        <span>Reason</span>
                        <input
                          aria-label="Flatten position reason"
                    placeholder="flatten position"
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

                  <section className="control-card control-card--span-5">
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
                            setCancelWorkingOrdersReason("cancel working orders");
                          }
                        })();
                      }}
                    >
                      <label className="field field--wide">
                        <span>Reason</span>
                        <input
                          aria-label="Cancel working orders reason"
                    placeholder="cancel working orders"
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
                    <p className="control-card__note">
                      Cancel routes only the current market&apos;s working orders through the
                      audited backend path.
                    </p>
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
              </ControlCluster>
            </div>
          </Panel>

          <RuntimeSummaryPanel snapshot={snapshot} />

          <ReadinessPanel snapshot={snapshot} readinessCounts={readinessCounts} />

          <HealthPanel snapshot={snapshot} feedStatuses={feedStatuses} />

          <HistoryPanel
            snapshot={snapshot}
            openWorkingOrders={openWorkingOrders}
            recentFills={recentFills}
            recentTrades={recentTrades}
            tradePerformance={tradePerformance}
            pnlChart={pnlChart}
            pnlChartPathData={pnlChartPathData}
            perTradePnl={perTradePnl}
            projectedPnlSnapshot={projectedPnlSnapshot}
          />

          <LatencyPanel
            snapshot={snapshot}
            latencyBreakdown={latencyBreakdown}
            slowestLatencyStage={slowestLatencyStage}
          />

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
                    placeholder="resolve reconnect review"
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
                    placeholder="resolve shutdown review"
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
              runtime host. The remaining work here is final operator polish and hands-on release
              verification.
            </p>
          </Panel>

          <JournalPanel
            snapshot={snapshot}
            journalSummary={journalSummary}
            journalRecords={journalRecords}
          />

          <EventsPanel
            eventFeed={eventFeed}
            eventHeadlineSummary={eventHeadlineSummary}
          />
        </div>
      ) : null}
    </main>
  );
}

export default App;
