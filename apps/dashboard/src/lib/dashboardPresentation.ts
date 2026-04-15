import type { BannerTone, LatencyStageViewModel } from "../dashboardModels";
import type {
  DecimalValue,
  EventJournalRecord,
  ReadinessCheckStatus,
  RuntimeReconnectReviewStatus,
  RuntimeShutdownReviewStatus,
  RuntimeStatusSnapshot,
  TradePathLatencyRecord,
  TradeSummaryRecord,
} from "../types/controlApi";

export function statusTone(status: ReadinessCheckStatus | BannerTone): BannerTone {
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

export function humanMemory(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "Unavailable";
  }

  const gibibytes = value / 1024 / 1024 / 1024;
  return `${gibibytes.toFixed(2)} GiB`;
}

export function reviewTone(
  review: RuntimeReconnectReviewStatus | RuntimeShutdownReviewStatus,
): BannerTone {
  if ("required" in review) {
    return review.required ? "warning" : "healthy";
  }

  return review.blocked || review.awaiting_flatten ? "warning" : "healthy";
}

export function latestLatency(status: RuntimeStatusSnapshot): number | null {
  return status.latest_trade_latency?.latency.end_to_end_fill_latency_ms ?? null;
}

export function reviewSummary(status: RuntimeStatusSnapshot): string {
  if (status.reconnect_review.required) {
    return "Reconnect review required";
  }

  if (status.shutdown_review.blocked || status.shutdown_review.awaiting_flatten) {
    return "Shutdown review pending";
  }

  return "No active safety review";
}

export function prettyJson(value: unknown): string {
  if (value === null || value === undefined) {
    return "No payload";
  }

  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "Payload unavailable";
  }
}

export function decimalToNumber(value: DecimalValue | null | undefined): number | null {
  if (value === null || value === undefined) {
    return null;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) ? value : null;
  }

  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function formatPercentage(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) {
    return "Unavailable";
  }

  return `${value.toFixed(1)}%`;
}

export function minutesBetween(
  startAt: string | null | undefined,
  endAt: string | null | undefined,
): number | null {
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

export function formatDurationMinutes(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) {
    return "Unavailable";
  }

  if (value < 60) {
    return `${value.toFixed(value >= 10 ? 0 : 1)} min`;
  }

  const hours = value / 60;
  return `${hours.toFixed(hours >= 10 ? 0 : 1)} hr`;
}

export function tradeTone(trade: TradeSummaryRecord): BannerTone {
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

export function journalRecordTone(record: EventJournalRecord): BannerTone {
  if (record.severity === "error") {
    return "danger";
  }

  if (record.severity === "warning") {
    return "warning";
  }

  return "info";
}

export function latencyStages(
  record: TradePathLatencyRecord | null | undefined,
): LatencyStageViewModel[] {
  const stages = [
    { key: "signal", label: "Signal", value: record?.latency.signal_latency_ms ?? null },
    { key: "decision", label: "Decision", value: record?.latency.decision_latency_ms ?? null },
    { key: "order-send", label: "Order send", value: record?.latency.order_send_latency_ms ?? null },
    { key: "broker-ack", label: "Broker ack", value: record?.latency.broker_ack_latency_ms ?? null },
    { key: "fill", label: "Fill", value: record?.latency.fill_latency_ms ?? null },
    {
      key: "sync-update",
      label: "Sync update",
      value: record?.latency.sync_update_latency_ms ?? null,
    },
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
