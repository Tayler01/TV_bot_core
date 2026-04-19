import type { DashboardSnapshot } from "./api";
import {
  decimalToNumber,
  minutesBetween,
  tradeTone,
} from "./dashboardPresentation";
import {
  formatDateTime,
  formatInteger,
  formatLatency,
  formatMode,
  formatSignedCurrency,
} from "./format";
import type {
  BannerTone,
  EventFeedItem,
  HeadlineSummary,
  JournalSummaryViewModel,
  PerTradePnlViewModel,
  PnlChartPoint,
  PnlChartViewModel,
  RuntimeSettingsDraft,
  TradePerformanceViewModel,
} from "../dashboardModels";
import type {
  ControlApiEvent,
  EventJournalRecord,
  FillRecord,
  OrderRecord,
  PnlSnapshotRecord,
  RuntimeEditableSettings,
  RuntimeLifecycleResponse,
  RuntimeMode,
  RuntimeSettingsSnapshot,
  RuntimeStatusSnapshot,
  RuntimeStrategyLibraryResponse,
  TradeSummaryRecord,
} from "../types/controlApi";

export const MAX_RECENT_EVENTS = 12;
export const MAX_RECENT_TRADES = 6;
export const MAX_RECENT_JOURNAL_RECORDS = 12;

export function modeTone(mode: RuntimeMode): "paper" | "live" | "neutral" {
  switch (mode) {
    case "paper":
      return "paper";
    case "live":
      return "live";
    default:
      return "neutral";
  }
}

export function feedbackToneFromHttpStatus(httpStatus: number): BannerTone {
  if (httpStatus >= 500) {
    return "danger";
  }

  if (httpStatus === 403 || httpStatus === 409 || httpStatus === 428) {
    return "warning";
  }

  return "healthy";
}

export function readinessSummary(counts: {
  pass: number;
  warning: number;
  blocking: number;
}) {
  if (counts.blocking > 0) {
    return `${counts.blocking} blocking`;
  }

  if (counts.warning > 0) {
    return `${counts.warning} warning`;
  }

  return "Ready";
}

export function readinessTone(counts: {
  pass: number;
  warning: number;
  blocking: number;
}): BannerTone {
  if (counts.blocking > 0) {
    return "danger";
  }

  if (counts.warning > 0) {
    return "warning";
  }

  return "healthy";
}

export function dispatchTone(status: RuntimeStatusSnapshot): BannerTone {
  if (status.command_dispatch_ready) {
    return "healthy";
  }

  return status.mode === "observation" ? "info" : "warning";
}

export function warmupTone(status: RuntimeStatusSnapshot["warmup_status"]): BannerTone {
  switch (status) {
    case "ready":
      return "healthy";
    case "failed":
      return "danger";
    default:
      return "warning";
  }
}

export function mergeLifecycleResponseIntoSnapshot(
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

export function selectStrategyPath(
  library: RuntimeStrategyLibraryResponse | null,
  currentPath: string,
): string {
  if (!library || library.strategies.length === 0) {
    return "";
  }

  if (library.strategies.some((entry) => entry.path === currentPath)) {
    return currentPath;
  }

  return (
    library.strategies.find((entry) => entry.valid)?.path ??
    library.strategies[0]?.path ??
    ""
  );
}

export function settingsDraftFromSnapshot(
  settings: RuntimeSettingsSnapshot,
): RuntimeSettingsDraft {
  return {
    startupMode: settings.editable.startup_mode,
    defaultStrategyPath: settings.editable.default_strategy_path ?? "",
    allowSqliteFallback: settings.editable.allow_sqlite_fallback,
    paperAccountName: settings.editable.paper_account_name ?? "",
    liveAccountName: settings.editable.live_account_name ?? "",
  };
}

export function runtimeSettingsRequestFromDraft(
  draft: RuntimeSettingsDraft,
): RuntimeEditableSettings {
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

export function reviewButtonDisabled(
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
      return event.snapshot.error_count > 0 || event.snapshot.feed_degraded
        ? "warning"
        : "healthy";
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
      return `Open trades ${formatInteger(
        event.projection.open_trade_ids.length,
      )} | Closed trades ${formatInteger(event.projection.closed_trade_count)}`;
    case "broker_status":
      return `${formatMode(event.snapshot.sync_state)} | ${event.snapshot.provider}`;
    case "journal_record":
      return compactJson(event.record.payload);
  }
}

export function toEventFeedItem(event: ControlApiEvent): EventFeedItem {
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

export function mergeEventIntoSnapshot(
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
        ...snapshot.journal.records.filter(
          (record) => record.event_id !== event.record.event_id,
        ),
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

export function workingOrdersForProjection(
  snapshot: DashboardSnapshot,
): OrderRecord[] {
  return snapshot.history.projection.working_order_ids
    .map((orderId) => snapshot.history.projection.orders[orderId])
    .filter((order): order is OrderRecord => Boolean(order))
    .sort((left, right) => compareDescendingDate(left.updated_at, right.updated_at));
}

function allTradeSummariesForProjection(
  snapshot: DashboardSnapshot,
): TradeSummaryRecord[] {
  return Object.values(snapshot.history.projection.trade_summaries).sort((left, right) =>
    compareDescendingDate(
      left.closed_at ?? left.opened_at,
      right.closed_at ?? right.opened_at,
    ),
  );
}

export function recentFillsForProjection(
  snapshot: DashboardSnapshot,
): FillRecord[] {
  return Object.values(snapshot.history.projection.fills)
    .sort((left, right) => compareDescendingDate(left.occurred_at, right.occurred_at))
    .slice(0, 6);
}

export function recentTradeSummariesForProjection(
  snapshot: DashboardSnapshot,
): TradeSummaryRecord[] {
  return allTradeSummariesForProjection(snapshot).slice(0, MAX_RECENT_TRADES);
}

export function recentJournalRecords(
  snapshot: DashboardSnapshot,
): EventJournalRecord[] {
  return snapshot.journal.records.slice(0, MAX_RECENT_JOURNAL_RECORDS);
}

export function tradePerformanceForProjection(
  snapshot: DashboardSnapshot,
): TradePerformanceViewModel {
  const trades = allTradeSummariesForProjection(snapshot);
  const closedTrades = trades.filter((trade) => trade.status === "closed");
  const winningTrades = closedTrades.filter(
    (trade) => (decimalToNumber(trade.net_pnl) ?? 0) > 0,
  );
  const losingTrades = closedTrades.filter(
    (trade) => (decimalToNumber(trade.net_pnl) ?? 0) < 0,
  );
  const averageNet =
    closedTrades.length > 0
      ? closedTrades.reduce(
          (total, trade) => total + (decimalToNumber(trade.net_pnl) ?? 0),
          0,
        ) / closedTrades.length
      : null;
  const holdingDurations = closedTrades
    .map((trade) => minutesBetween(trade.opened_at, trade.closed_at))
    .filter((value): value is number => value !== null);
  const averageHoldMinutes =
    holdingDurations.length > 0
      ? holdingDurations.reduce((total, value) => total + value, 0) /
        holdingDurations.length
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

export function pnlChartForProjection(
  snapshot: DashboardSnapshot,
): PnlChartViewModel {
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

  const currentPnlSnapshot = snapshot.history.projection.latest_pnl_snapshot;
  if (currentPnlSnapshot) {
    rawPoints.push({
      id: currentPnlSnapshot.snapshot_id,
      label: "Now",
      note: `Floating now at ${formatDateTime(currentPnlSnapshot.captured_at)}`,
      value: decimalToNumber(currentPnlSnapshot.net_pnl) ?? cumulativeNet,
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

export function pnlChartPath(points: PnlChartPoint[]): string {
  return points
    .map((point, index) =>
      `${index === 0 ? "M" : "L"} ${point.xPercent.toFixed(1)} ${point.yPercent.toFixed(1)}`,
    )
    .join(" ");
}

export function perTradePnlForProjection(
  snapshot: DashboardSnapshot,
): PerTradePnlViewModel[] {
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

export function summarizeJournalRecords(
  records: EventJournalRecord[],
): JournalSummaryViewModel {
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

export function summarizeRecentEvents(
  events: EventFeedItem[],
): HeadlineSummary[] {
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

export function latestPnlSnapshot(
  snapshot: DashboardSnapshot,
): PnlSnapshotRecord | null {
  return snapshot.history.projection.latest_pnl_snapshot;
}

export function isPositiveNumberInput(value: string): boolean {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0;
}
