import type {
  CandlestickData,
  HistogramData,
  SeriesMarker,
  UTCTimestamp,
} from "lightweight-charts";

import type {
  BrokerFillUpdate,
  RuntimeChartBar,
  RuntimeChartSnapshot,
  Timeframe,
} from "../types/controlApi";

export interface ChartPriceLineDescriptor {
  key: string;
  price: number;
  color: string;
  title: string;
  lineStyle?: number;
  axisLabelColor?: string;
  axisLabelTextColor?: string;
}

export function chartTimeframeLabel(timeframe: Timeframe): string {
  switch (timeframe) {
    case "1s":
      return "1 second";
    case "1m":
      return "1 minute";
    case "5m":
      return "5 minute";
    default:
      return timeframe;
  }
}

export function decimalToNumber(value: number | string | null | undefined): number | null {
  if (value === null || value === undefined || value === "") {
    return null;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) ? value : null;
  }

  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function toUtcTimestamp(value: string): UTCTimestamp {
  return Math.floor(Date.parse(value) / 1000) as UTCTimestamp;
}

export function toChartCandles(
  bars: RuntimeChartBar[],
): CandlestickData<UTCTimestamp>[] {
  return bars
    .map((bar) => {
      const open = decimalToNumber(bar.open);
      const high = decimalToNumber(bar.high);
      const low = decimalToNumber(bar.low);
      const close = decimalToNumber(bar.close);

      if (open === null || high === null || low === null || close === null) {
        return null;
      }

      return {
        time: toUtcTimestamp(bar.closed_at),
        open,
        high,
        low,
        close,
      } satisfies CandlestickData<UTCTimestamp>;
    })
    .filter((bar): bar is CandlestickData<UTCTimestamp> => bar !== null);
}

export function toVolumeHistogram(
  bars: RuntimeChartBar[],
): HistogramData<UTCTimestamp>[] {
  return bars.map((bar) => {
    const open = decimalToNumber(bar.open) ?? 0;
    const close = decimalToNumber(bar.close) ?? 0;

    return {
      time: toUtcTimestamp(bar.closed_at),
      value: bar.volume,
      color:
        close >= open
          ? "rgba(126, 225, 163, 0.38)"
          : "rgba(255, 143, 127, 0.34)",
    } satisfies HistogramData<UTCTimestamp>;
  });
}

function nearestMarkerTime(
  bars: RuntimeChartBar[],
  occurredAt: string,
): UTCTimestamp | null {
  if (bars.length === 0) {
    return null;
  }

  const occurredAtMs = Date.parse(occurredAt);
  let selected = bars[0];

  for (const bar of bars) {
    if (Date.parse(bar.closed_at) > occurredAtMs) {
      break;
    }

    selected = bar;
  }

  return toUtcTimestamp(selected.closed_at);
}

export function toFillMarkers(
  snapshot: RuntimeChartSnapshot | null,
): SeriesMarker<UTCTimestamp>[] {
  if (!snapshot) {
    return [];
  }

  return snapshot.recent_fills
    .map((fill) => fillMarkerForSnapshotFill(snapshot.bars, fill))
    .filter((marker): marker is SeriesMarker<UTCTimestamp> => marker !== null);
}

function fillMarkerForSnapshotFill(
  bars: RuntimeChartBar[],
  fill: BrokerFillUpdate,
): SeriesMarker<UTCTimestamp> | null {
  const time = nearestMarkerTime(bars, fill.occurred_at);
  const price = decimalToNumber(fill.price);

  if (time === null || price === null) {
    return null;
  }

  const quantity = fill.quantity.toString();

  return {
    time,
    position: fill.side === "buy" ? "atPriceBottom" : "atPriceTop",
    price,
    shape: fill.side === "buy" ? "arrowUp" : "arrowDown",
    color: fill.side === "buy" ? "#7ee1a3" : "#ff8f7f",
    text: `${fill.side === "buy" ? "B" : "S"} ${quantity}`,
  };
}

export function chartPriceLines(
  snapshot: RuntimeChartSnapshot | null,
): ChartPriceLineDescriptor[] {
  if (!snapshot?.active_position) {
    return [];
  }

  const averagePrice = decimalToNumber(snapshot.active_position.average_price);

  if (averagePrice === null) {
    return [];
  }

  return [
    {
      key: "active-position",
      price: averagePrice,
      color: "#58c0ff",
      title:
        snapshot.active_position.quantity > 0
          ? `Long ${snapshot.active_position.quantity}`
          : `Short ${Math.abs(snapshot.active_position.quantity)}`,
      axisLabelColor: "#0f2437",
      axisLabelTextColor: "#edf4ff",
    },
  ];
}

export function mergeChartBars(
  existing: RuntimeChartBar[],
  incoming: RuntimeChartBar[],
): RuntimeChartBar[] {
  const byClosedAt = new Map<string, RuntimeChartBar>();

  for (const bar of existing) {
    byClosedAt.set(bar.closed_at, bar);
  }

  for (const bar of incoming) {
    byClosedAt.set(bar.closed_at, bar);
  }

  return Array.from(byClosedAt.values()).sort((left, right) =>
    left.closed_at.localeCompare(right.closed_at),
  );
}
