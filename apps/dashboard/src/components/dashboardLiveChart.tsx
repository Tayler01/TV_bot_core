import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  CandlestickSeries,
  ColorType,
  createChart,
  CrosshairMode,
  LineStyle,
  type IChartApi,
  type IPriceLine,
  type ISeriesApi,
  type Time,
} from "lightweight-charts";

import type { ChartViewModel } from "../dashboardModels";
import {
  chartPriceLines,
  chartTimeframeLabel,
  decimalToNumber,
  toChartCandles,
} from "../lib/chartAdapter";
import {
  formatDateTime,
  formatDecimal,
  formatInteger,
  formatMode,
} from "../lib/format";
import type {
  RuntimeChartSnapshot,
  RuntimeStatusSnapshot,
  Timeframe,
} from "../types/controlApi";
import { Panel, Pill } from "./dashboardPrimitives";

const CHART_HEIGHT = 560;
const CHART_INITIAL_FIT_TOKEN = 0;
const CHART_PREFETCH_THRESHOLD_BARS = 36;
const SVG_FALLBACK_MIN_VISIBLE_BARS = 64;

interface ChartOperationalAlert {
  id: string;
  tone: "healthy" | "warning" | "danger" | "info";
  headline: string;
  detail: string;
}

function compactAlertDetail(detail: string | null | undefined, fallback: string): string {
  const trimmed = detail?.replace(/\s+/g, " ").trim();

  if (!trimmed) {
    return fallback;
  }

  if (/missing broker configuration/i.test(trimmed)) {
    return "Broker credentials missing.";
  }

  if (/authentication failed/i.test(trimmed)) {
    return "Databento authentication failed.";
  }

  if (/polling failed/i.test(trimmed) || /next_record/i.test(trimmed)) {
    return "Databento stream disconnected.";
  }

  if (/missing market data/i.test(trimmed) || /api key/i.test(trimmed)) {
    return "Databento key missing.";
  }

  if (trimmed.length <= 96) {
    return trimmed;
  }

  return `${trimmed.slice(0, 93).trimEnd()}...`;
}

function chartStreamLabel(streamState: ChartViewModel["streamState"]) {
  switch (streamState) {
    case "open":
      return "Live";
    case "connecting":
      return "Syncing";
    case "unsupported":
      return "Unsupported";
    case "closed":
      return "Offline";
    case "error":
      return "Error";
    default:
      return "Waiting";
  }
}

function chartStreamTone(streamState: ChartViewModel["streamState"]) {
  switch (streamState) {
    case "open":
      return "healthy";
    case "connecting":
      return "info";
    case "unsupported":
    case "closed":
      return "warning";
    case "error":
      return "danger";
    default:
      return "info";
  }
}

function healthTone(health: string | null) {
  switch (health) {
    case "healthy":
      return "healthy";
    case "failed":
      return "danger";
    case "degraded":
      return "warning";
    default:
      return "info";
  }
}

function timeframeButtonLabel(timeframe: Timeframe): string {
  switch (timeframe) {
    case "1s":
      return "1s";
    case "1m":
      return "1m";
    case "5m":
      return "5m";
    default:
      return timeframe;
  }
}

function preferredVisibleBars(timeframe: Timeframe, chartWidth: number): number {
  if (timeframe === "1s") {
    if (chartWidth >= 1100) {
      return 146;
    }

    if (chartWidth >= 820) {
      return 116;
    }

    return 72;
  }

  if (timeframe === "5m") {
    if (chartWidth >= 1100) {
      return 108;
    }

    if (chartWidth >= 820) {
      return 88;
    }

    return 56;
  }

  if (chartWidth >= 1100) {
    return 128;
  }

  if (chartWidth >= 820) {
    return 102;
  }

  return 64;
}

function applyPreferredViewport(
  chart: IChartApi,
  timeframe: Timeframe,
  candleCount: number,
  chartWidth: number,
) {
  if (candleCount <= 0) {
    return;
  }

  const visibleBars = preferredVisibleBars(timeframe, chartWidth);
  const rightPadding = Math.max(4, Math.round(visibleBars * 0.06));
  const from = Math.max(candleCount - visibleBars, -1);
  const to = candleCount - 1 + rightPadding;

  chart.timeScale().setVisibleLogicalRange({ from, to });
}

function chartHostHasInk(host: HTMLDivElement | null): boolean {
  if (!host) {
    return false;
  }

  const canvases = Array.from(host.querySelectorAll("canvas"));

  if (canvases.length === 0) {
    return false;
  }

  for (const canvas of canvases) {
    if (canvas.width <= 0 || canvas.height <= 0) {
      continue;
    }

    const context = canvas.getContext("2d", { willReadFrequently: true });

    if (!context) {
      continue;
    }

    const stepX = Math.max(Math.floor(canvas.width / 18), 1);
    const stepY = Math.max(Math.floor(canvas.height / 10), 1);

    for (let y = 0; y < canvas.height; y += stepY) {
      for (let x = 0; x < canvas.width; x += stepX) {
        const [red, green, blue, alpha] = context.getImageData(x, y, 1, 1).data;

        if (alpha > 0 && (red > 12 || green > 12 || blue > 12)) {
          return true;
        }
      }
    }
  }

  return false;
}

function svgFallbackVisibleBars(
  snapshot: RuntimeChartSnapshot,
  timeframe: Timeframe,
  chartWidth: number,
) {
  const visibleBars = Math.max(
    preferredVisibleBars(timeframe, chartWidth),
    SVG_FALLBACK_MIN_VISIBLE_BARS,
  );

  if (snapshot.bars.length <= visibleBars) {
    return snapshot.bars;
  }

  return snapshot.bars.slice(-visibleBars);
}

function SvgCandlestickFallback({
  snapshot,
  timeframe,
  chartWidth,
}: {
  snapshot: RuntimeChartSnapshot;
  timeframe: Timeframe;
  chartWidth: number;
}) {
  const width = Math.max(chartWidth, 320);
  const height = CHART_HEIGHT;
  const paddingTop = 18;
  const paddingBottom = 26;
  const paddingLeft = 14;
  const paddingRight = 64;
  const plotWidth = Math.max(width - paddingLeft - paddingRight, 220);
  const plotHeight = Math.max(height - paddingTop - paddingBottom, 220);
  const bars = svgFallbackVisibleBars(snapshot, timeframe, width);
  const highs = bars
    .map((bar) => decimalToNumber(bar.high))
    .filter((value): value is number => value !== null);
  const lows = bars
    .map((bar) => decimalToNumber(bar.low))
    .filter((value): value is number => value !== null);

  if (!highs.length || !lows.length) {
    return null;
  }

  const high = Math.max(...highs);
  const low = Math.min(...lows);
  const spread = Math.max(high - low, 0.5);
  const pricePadding = spread * 0.12;
  const maxPrice = high + pricePadding;
  const minPrice = low - pricePadding;
  const slotWidth = plotWidth / Math.max(bars.length, 1);
  const candleWidth = Math.max(Math.min(slotWidth * 0.56, 12), 2);
  const gridLevels = 5;

  const yForPrice = (price: number) =>
    paddingTop + ((maxPrice - price) / Math.max(maxPrice - minPrice, 0.0001)) * plotHeight;

  const xForIndex = (index: number) => paddingLeft + slotWidth * index + slotWidth / 2;

  const priceLines = chartPriceLines(snapshot)
    .filter((line) => line.price >= minPrice && line.price <= maxPrice)
    .map((line) => ({
      ...line,
      y: yForPrice(line.price),
    }));

  return (
    <svg
      aria-label="Contract chart fallback"
      className="live-chart__svg-fallback"
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
    >
      <rect x="0" y="0" width={width} height={height} fill="rgba(9, 17, 28, 0.96)" />
      {Array.from({ length: gridLevels }).map((_, index) => {
        const price = maxPrice - ((maxPrice - minPrice) / (gridLevels - 1)) * index;
        const y = yForPrice(price);
        return (
          <g key={`grid-${index}`}>
            <line
              x1={paddingLeft}
              y1={y}
              x2={width - paddingRight}
              y2={y}
              stroke="rgba(140, 167, 205, 0.14)"
              strokeWidth="1"
            />
            <text
              x={width - paddingRight + 8}
              y={y + 4}
              fill="#8fa6c7"
              fontSize="11"
              fontFamily="'IBM Plex Mono', monospace"
            >
              {formatDecimal(price)}
            </text>
          </g>
        );
      })}
      {priceLines.map((line) => (
        <g key={line.key}>
          <line
            x1={paddingLeft}
            y1={line.y}
            x2={width - paddingRight}
            y2={line.y}
            stroke={line.color}
            strokeWidth="1.25"
            strokeDasharray={line.lineStyle === LineStyle.Dotted ? "2 4" : "6 6"}
            opacity="0.9"
          />
          <text
            x={width - paddingRight + 8}
            y={line.y - 4}
            fill={line.color}
            fontSize="10"
            fontFamily="'IBM Plex Mono', monospace"
          >
            {line.title}
          </text>
        </g>
      ))}
      {bars.map((bar, index) => {
        const open = decimalToNumber(bar.open);
        const highValue = decimalToNumber(bar.high);
        const lowValue = decimalToNumber(bar.low);
        const close = decimalToNumber(bar.close);

        if (
          open === null ||
          highValue === null ||
          lowValue === null ||
          close === null
        ) {
          return null;
        }

        const x = xForIndex(index);
        const wickTop = yForPrice(highValue);
        const wickBottom = yForPrice(lowValue);
        const bodyTop = yForPrice(Math.max(open, close));
        const bodyBottom = yForPrice(Math.min(open, close));
        const rising = close >= open;
        const color = rising ? "#7ee1a3" : "#ff8f7f";
        const bodyHeight = Math.max(bodyBottom - bodyTop, 1.5);

        return (
          <g key={`${bar.closed_at}-${index}`}>
            <line
              x1={x}
              y1={wickTop}
              x2={x}
              y2={wickBottom}
              stroke={color}
              strokeWidth="1.4"
              strokeLinecap="round"
            />
            <rect
              x={x - candleWidth / 2}
              y={bodyTop}
              width={candleWidth}
              height={bodyHeight}
              rx="1.4"
              fill={color}
              opacity={rising ? 0.95 : 0.88}
            />
          </g>
        );
      })}
      <text
        x={paddingLeft}
        y={height - 8}
        fill="#8fa6c7"
        fontSize="11"
        fontFamily="'IBM Plex Mono', monospace"
      >
        {`${bars.length} bars | ${chartTimeframeLabel(timeframe)} | fallback renderer`}
      </text>
      <text
        x={width - paddingRight}
        y={height - 8}
        fill="#8fa6c7"
        fontSize="11"
        textAnchor="end"
        fontFamily="'IBM Plex Mono', monospace"
      >
        {formatDateTime(bars[bars.length - 1]?.closed_at ?? null)}
      </text>
    </svg>
  );
}

function LiveChartCanvas({
  chartViewModel,
  fitRequestToken,
  liveFollowEnabled,
  onRequestOlderHistory,
}: {
  chartViewModel: ChartViewModel;
  fitRequestToken: number;
  liveFollowEnabled: boolean;
  onRequestOlderHistory: () => void;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartHostRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<"Candlestick", Time> | null>(null);
  const priceLinesRef = useRef<IPriceLine[]>([]);
  const previousTimeframeRef = useRef<Timeframe | null>(null);
  const previousLatestClosedAtRef = useRef<string | null>(null);
  const previousBarCountRef = useRef(0);
  const lastPrefetchCursorRef = useRef<string | null>(null);
  const paintCheckFrameRef = useRef<number | null>(null);
  const [chartWidth, setChartWidth] = useState(320);
  const [fallbackVisible, setFallbackVisible] = useState(false);

  useLayoutEffect(() => {
    const container = chartHostRef.current;

    if (!container) {
      return;
    }

    const chart = createChart(container, {
      width: Math.max(container.clientWidth, 320),
      height: CHART_HEIGHT,
      layout: {
        background: {
          type: ColorType.Solid,
          color: "#09111c",
        },
        textColor: "#d7e3f5",
        attributionLogo: false,
      },
      grid: {
        vertLines: {
          color: "rgba(140, 167, 205, 0.10)",
        },
        horzLines: {
          color: "rgba(140, 167, 205, 0.10)",
        },
      },
      crosshair: {
        mode: CrosshairMode.Normal,
        vertLine: {
          color: "rgba(88, 192, 255, 0.28)",
          width: 1,
          style: LineStyle.Dashed,
        },
        horzLine: {
          color: "rgba(255, 180, 84, 0.22)",
          width: 1,
          style: LineStyle.Dotted,
        },
      },
      rightPriceScale: {
        borderColor: "rgba(140, 167, 205, 0.18)",
      },
      timeScale: {
        borderColor: "rgba(140, 167, 205, 0.18)",
        timeVisible: true,
        secondsVisible: chartViewModel.selectedTimeframe === "1s",
      },
      localization: {
        timeFormatter: (time: Time) => {
          const milliseconds =
            typeof time === "number" ? time * 1000 : Date.parse(String(time));
          return new Intl.DateTimeFormat(undefined, {
            month: "short",
            day: "numeric",
            hour: "numeric",
            minute: "2-digit",
            second:
              chartViewModel.selectedTimeframe === "1s" ? "2-digit" : undefined,
          }).format(milliseconds);
        },
      },
    });

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: "#7ee1a3",
      borderUpColor: "#7ee1a3",
      wickUpColor: "#7ee1a3",
      downColor: "#ff8f7f",
      borderDownColor: "#ff8f7f",
      wickDownColor: "#ff8f7f",
      priceLineColor: "#58c0ff",
      priceLineVisible: true,
    });

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;

    let resizeFrame = 0;
    const resizeChart = () => {
      const nextWidth = Math.max(containerRef.current?.clientWidth ?? container.clientWidth, 320);
      setChartWidth(nextWidth);
      chart.resize(nextWidth, CHART_HEIGHT);
    };
    const scheduleResize = () => {
      if (resizeFrame !== 0) {
        return;
      }

      resizeFrame = window.requestAnimationFrame(() => {
        resizeFrame = 0;
        resizeChart();
      });
    };

    resizeChart();
    window.addEventListener("resize", scheduleResize);
    const resizeObserver =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => {
            scheduleResize();
          })
        : null;
    resizeObserver?.observe(containerRef.current ?? container);

    return () => {
      if (paintCheckFrameRef.current !== null) {
        window.cancelAnimationFrame(paintCheckFrameRef.current);
        paintCheckFrameRef.current = null;
      }
      if (resizeFrame !== 0) {
        window.cancelAnimationFrame(resizeFrame);
        resizeFrame = 0;
      }
      window.removeEventListener("resize", scheduleResize);
      resizeObserver?.disconnect();

      chart.remove();
      chartRef.current = null;
      candleSeriesRef.current = null;
      priceLinesRef.current = [];
    };
  }, []);

  useLayoutEffect(() => {
    chartRef.current?.applyOptions({
      timeScale: {
        secondsVisible: chartViewModel.selectedTimeframe === "1s",
      },
    });
  }, [chartViewModel.selectedTimeframe]);

  useLayoutEffect(() => {
    const chart = chartRef.current;
    const candleSeries = candleSeriesRef.current;

    if (!chart || !candleSeries) {
      return;
    }

    const snapshot = chartViewModel.snapshot;
    const candles = snapshot ? toChartCandles(snapshot.bars) : [];

    candleSeries.setData(candles);

    for (const line of priceLinesRef.current) {
      candleSeries.removePriceLine(line);
    }
    priceLinesRef.current = [];

    for (const line of chartPriceLines(snapshot)) {
      priceLinesRef.current.push(
        candleSeries.createPriceLine({
          price: line.price,
          color: line.color,
          title: line.title,
          lineWidth: 2,
          lineStyle: line.lineStyle ?? LineStyle.Dashed,
          axisLabelVisible: true,
          axisLabelColor: line.axisLabelColor,
          axisLabelTextColor: line.axisLabelTextColor,
        }),
      );
    }

    if (candles.length > 0 && chartViewModel.selectedTimeframe) {
      const timeframeChanged =
        previousTimeframeRef.current !== chartViewModel.selectedTimeframe;
      const firstRender = previousBarCountRef.current === 0;
      const latestClosedAt = snapshot?.latest_closed_at ?? null;
      const newTailBar =
        latestClosedAt !== null && latestClosedAt !== previousLatestClosedAtRef.current;

      if (timeframeChanged || firstRender) {
        applyPreferredViewport(
          chart,
          chartViewModel.selectedTimeframe,
          candles.length,
          Math.max(containerRef.current?.clientWidth ?? 0, 320),
        );
      } else if (liveFollowEnabled && newTailBar) {
        chart.timeScale().scrollToRealTime();
      }

      previousTimeframeRef.current = chartViewModel.selectedTimeframe;
      previousLatestClosedAtRef.current = latestClosedAt;
      previousBarCountRef.current = candles.length;
    } else {
      previousTimeframeRef.current = chartViewModel.selectedTimeframe;
      previousLatestClosedAtRef.current = null;
      previousBarCountRef.current = 0;
    }

    if (paintCheckFrameRef.current !== null) {
      window.cancelAnimationFrame(paintCheckFrameRef.current);
      paintCheckFrameRef.current = null;
    }

    if (!candles.length) {
      setFallbackVisible(false);
      return;
    }

    setFallbackVisible(false);
    paintCheckFrameRef.current = window.requestAnimationFrame(() => {
      paintCheckFrameRef.current = window.requestAnimationFrame(() => {
        paintCheckFrameRef.current = null;
        setFallbackVisible(!chartHostHasInk(chartHostRef.current));
      });
    });
  }, [chartViewModel.selectedTimeframe, chartViewModel.snapshot, liveFollowEnabled]);

  useEffect(() => {
    if (fitRequestToken === CHART_INITIAL_FIT_TOKEN) {
      return;
    }

    const chart = chartRef.current;
    const bars = chartViewModel.snapshot?.bars.length ?? 0;

    if (!chart || bars === 0 || !chartViewModel.selectedTimeframe) {
      return;
    }

    applyPreferredViewport(
      chart,
      chartViewModel.selectedTimeframe,
      bars,
      Math.max(containerRef.current?.clientWidth ?? 0, 320),
    );
  }, [fitRequestToken, chartViewModel.selectedTimeframe, chartViewModel.snapshot?.bars.length]);

  useEffect(() => {
    if (!liveFollowEnabled || !chartViewModel.snapshot?.bars.length) {
      return;
    }

    chartRef.current?.timeScale().scrollToRealTime();
  }, [chartViewModel.snapshot?.bars.length, liveFollowEnabled]);

  useEffect(() => {
    const chart = chartRef.current;
    const candleSeries = candleSeriesRef.current;

    if (!chart || !candleSeries) {
      return;
    }

    const evaluateRange = () => {
      const snapshot = chartViewModel.snapshot;

      if (
        !snapshot?.bars.length ||
        !snapshot.can_load_older_history ||
        chartViewModel.historyState === "loading"
      ) {
        return;
      }

      const visibleRange = chart.timeScale().getVisibleLogicalRange();

      if (!visibleRange) {
        return;
      }

      const barsInfo = candleSeries.barsInLogicalRange(visibleRange);
      const earliestLoadedBar = snapshot.bars[0]?.closed_at ?? null;

      if (!barsInfo || !earliestLoadedBar) {
        return;
      }

      if (
        barsInfo.barsBefore < CHART_PREFETCH_THRESHOLD_BARS &&
        lastPrefetchCursorRef.current !== earliestLoadedBar
      ) {
        lastPrefetchCursorRef.current = earliestLoadedBar;
        onRequestOlderHistory();
      }
    };

    const handleVisibleRangeChange = () => {
      evaluateRange();
    };

    chart.timeScale().subscribeVisibleLogicalRangeChange(handleVisibleRangeChange);
    const frame = window.requestAnimationFrame(() => {
      evaluateRange();
    });

    return () => {
      window.cancelAnimationFrame(frame);
      chart.timeScale().unsubscribeVisibleLogicalRangeChange(handleVisibleRangeChange);
    };
  }, [
    chartViewModel.historyState,
    chartViewModel.snapshot,
    onRequestOlderHistory,
  ]);

  const fallbackEligible =
    fallbackVisible &&
    chartViewModel.snapshot !== null &&
    chartViewModel.selectedTimeframe !== null;
  const fallbackSnapshot = fallbackEligible ? chartViewModel.snapshot : null;
  const fallbackTimeframe = fallbackEligible ? chartViewModel.selectedTimeframe : null;

  return (
    <div ref={containerRef} className="live-chart__canvas">
      <div
        ref={chartHostRef}
        className={`live-chart__canvas-host${fallbackEligible ? " live-chart__canvas-host--hidden" : ""}`}
      />
      {fallbackSnapshot && fallbackTimeframe ? (
        <div className="live-chart__canvas-overlay">
          <SvgCandlestickFallback
            snapshot={fallbackSnapshot}
            timeframe={fallbackTimeframe}
            chartWidth={chartWidth}
          />
        </div>
      ) : null}
    </div>
  );
}

function workingOrderLevelsSummary(order: {
  limit_price: number | string | null;
  stop_price: number | string | null;
}) {
  const levels: string[] = [];

  if (order.limit_price !== null && order.limit_price !== undefined) {
    levels.push(`limit ${formatDecimal(order.limit_price)}`);
  }

  if (order.stop_price !== null && order.stop_price !== undefined) {
    levels.push(`stop ${formatDecimal(order.stop_price)}`);
  }

  return levels.length ? levels.join(" | ") : "working price unavailable";
}

function activePositionSummary(
  activePosition: {
    quantity: number;
    average_price: number | string | null;
  } | null,
) {
  if (!activePosition) {
    return "No active position";
  }

  const side = activePosition.quantity >= 0 ? "Long" : "Short";
  return `${side} ${formatInteger(Math.abs(activePosition.quantity))} @ ${formatDecimal(activePosition.average_price)}`;
}

function workingOrderSummary(
  workingOrders: Array<{
    side: string | null;
    quantity: number | null;
    limit_price: number | string | null;
    stop_price: number | string | null;
  }>,
) {
  if (!workingOrders.length) {
    return "No working orders";
  }

  const primaryOrder = workingOrders[0];
  return `${workingOrders.length} active | ${primaryOrder.side ?? "side?"} ${formatInteger(primaryOrder.quantity)} | ${workingOrderLevelsSummary(primaryOrder)}`;
}

function chartOperationalAlerts(
  runtimeStatus: RuntimeStatusSnapshot | null,
  chartViewModel: ChartViewModel,
): ChartOperationalAlert[] {
  const alerts: ChartOperationalAlert[] = [];
  const marketData = runtimeStatus?.market_data_status?.session.market_data ?? null;
  const chartConfig = chartViewModel.config;

  if (runtimeStatus?.reconnect_review.required) {
    alerts.push({
      id: "reconnect-review",
      tone: "warning",
      headline: "Reconnect review",
      detail: compactAlertDetail(
        runtimeStatus.reconnect_review.reason,
        `${runtimeStatus.reconnect_review.open_position_count} pos | ${runtimeStatus.reconnect_review.working_order_count} orders pending review.`,
      ),
    });
  }

  if (runtimeStatus?.shutdown_review.blocked) {
    alerts.push({
      id: "shutdown-review",
      tone: "warning",
      headline: "Shutdown review",
      detail: compactAlertDetail(
        runtimeStatus.shutdown_review.reason,
        "Resolve broker exposure before shutdown completes.",
      ),
    });
  }

  if (
    marketData?.health === "failed" ||
    marketData?.health === "degraded" ||
    runtimeStatus?.system_health?.feed_degraded
  ) {
    alerts.push({
      id: "feed-degraded",
      tone: marketData?.health === "failed" ? "danger" : "warning",
      headline: "Feed degraded",
      detail: compactAlertDetail(
        runtimeStatus?.market_data_detail ?? marketData?.last_disconnect_reason,
        "Entries stay blocked until the feed recovers.",
      ),
    });
  }

  if (chartConfig?.sample_data_active) {
    alerts.push({
      id: "sample-candles",
      tone: "info",
      headline: "Sample candles",
      detail: "Showing sample candles until Databento is configured.",
    });
  }

  if (chartViewModel.streamState === "error" || chartViewModel.streamState === "closed") {
    alerts.push({
      id: "chart-stream",
      tone: chartViewModel.streamState === "error" ? "danger" : "warning",
      headline: "Stream reconnecting",
      detail: compactAlertDetail(
        chartViewModel.error,
        "Buffered history stays visible while live updates reconnect.",
      ),
    });
  } else if (chartViewModel.streamState === "connecting") {
    alerts.push({
      id: "chart-stream-connecting",
      tone: "info",
      headline: "Stream connecting",
      detail: "Snapshot data stays visible while live updates sync.",
    });
  }

  if (runtimeStatus && !runtimeStatus.command_dispatch_ready) {
    alerts.push({
      id: "dispatch-unavailable",
      tone: "warning",
      headline: "Dispatch off",
      detail: compactAlertDetail(runtimeStatus.command_dispatch_detail, "Runtime dispatch unavailable."),
    });
  }

  return alerts;
}

export function LiveChartPanel({
  chartViewModel,
  runtimeStatus,
  onSelectTimeframe,
  onLoadOlderHistory,
  onRefreshChart,
}: {
  chartViewModel: ChartViewModel;
  runtimeStatus: RuntimeStatusSnapshot | null;
  onSelectTimeframe: (timeframe: Timeframe) => void;
  onLoadOlderHistory: () => void;
  onRefreshChart: () => void;
}) {
  const config = chartViewModel.config;
  const snapshot = chartViewModel.snapshot;
  const instrument = config?.instrument ?? null;
  const activePosition = snapshot?.active_position ?? null;
  const workingOrders = snapshot?.working_orders ?? [];
  const recentFills = snapshot?.recent_fills ?? [];
  const latestClosedAt = snapshot?.latest_closed_at ?? null;

  const latestBarSummary = useMemo(() => {
    if (!snapshot?.bars.length) {
      return null;
    }

    return snapshot.bars[snapshot.bars.length - 1];
  }, [snapshot]);
  const latestBarBuilding = latestBarSummary ? !latestBarSummary.is_complete : false;
  const latestBarTimestampLabel = latestClosedAt
    ? latestBarBuilding
      ? `Build ${formatDateTime(latestClosedAt)}`
      : formatDateTime(latestClosedAt)
    : "No candle closed yet";
  const operationalAlerts = useMemo(
    () => chartOperationalAlerts(runtimeStatus, chartViewModel),
    [chartViewModel, runtimeStatus],
  );
  const [liveFollowEnabled, setLiveFollowEnabled] = useState(true);
  const [fitRequestToken, setFitRequestToken] = useState(CHART_INITIAL_FIT_TOKEN);
  const contractHeadline =
    instrument?.tradovate_symbol ?? instrument?.canonical_symbol ?? "Chart unavailable";
  const contractSubline = instrument?.strategy_name
    ? `${instrument.strategy_name}${instrument.canonical_symbol ? ` | ${instrument.canonical_symbol}` : ""}`
    : config?.detail ?? "Waiting for chart config";

  return (
    <Panel
      className="panel--full panel--chart"
      eyebrow="Chart"
      title="Contract chart"
      hideHeading
    >
      <div className="chart-toolbar chart-toolbar--primary">
        <div className="chart-toolbar__identity">
          <strong>{contractHeadline}</strong>
          <div className="chart-toolbar__identity-meta">
            <span>{contractSubline}</span>
          </div>
        </div>
        <div className="chart-toolbar__group chart-toolbar__group--status">
          <Pill
            label={
              config?.available
                ? `Feed ${config?.market_data_health ?? "unknown"}`
                : "Chart unavailable"
            }
            tone={config?.available ? healthTone(config?.market_data_health ?? null) : "warning"}
          />
          <Pill
            label={chartStreamLabel(chartViewModel.streamState)}
            tone={chartStreamTone(chartViewModel.streamState)}
          />
          {config?.sample_data_active ? (
            <Pill label="Sample" tone="info" />
          ) : null}
          <Pill
            label={config?.trade_ready ? "Ready" : "Warmup"}
            tone={config?.trade_ready ? "healthy" : "warning"}
          />
        </div>
        <div className="chart-toolbar__group chart-toolbar__group--actions">
          <button
            className={
              liveFollowEnabled
                ? "command-button command-button--active"
                : "command-button"
            }
            type="button"
            aria-pressed={liveFollowEnabled}
            onClick={() => {
              setLiveFollowEnabled((current) => !current);
            }}
          >
            {liveFollowEnabled ? "Auto" : "Manual"}
          </button>
          <button
            className="command-button"
            type="button"
            onClick={() => {
              setFitRequestToken((current) => current + 1);
            }}
          >
            Fit
          </button>
          <button
            className="command-button"
            type="button"
            onClick={onRefreshChart}
          >
            Sync
          </button>
          <button
            className="command-button"
            type="button"
            onClick={onLoadOlderHistory}
            disabled={
              !snapshot?.can_load_older_history || chartViewModel.historyState === "loading"
            }
          >
            {chartViewModel.historyState === "loading" ? "Loading" : "Older"}
          </button>
        </div>
      </div>

      <div className="chart-toolbar chart-toolbar--timeframes">
        <div className="chart-timeframes" role="toolbar" aria-label="Chart timeframe">
          {(config?.supported_timeframes ?? []).map((timeframe) => (
            <button
              key={timeframe}
              className={
                timeframe === chartViewModel.selectedTimeframe
                  ? "chart-timeframe chart-timeframe--active"
                  : "chart-timeframe"
              }
              type="button"
              onClick={() => {
                onSelectTimeframe(timeframe);
              }}
            >
              {timeframeButtonLabel(timeframe)}
            </button>
          ))}
        </div>
        <div className="chart-toolbar__group chart-toolbar__group--actions">
          <Pill
            label={chartViewModel.selectedTimeframe ? chartTimeframeLabel(chartViewModel.selectedTimeframe) : "Timeframe waiting"}
            tone="info"
          />
          <Pill
            label={`${formatInteger(snapshot?.bars.length ?? 0)} bars`}
            tone="info"
          />
          <Pill
            label={
              config?.sample_data_active
                ? "Sample data"
                : config?.replay_caught_up
                  ? "Replay ok"
                  : "Replay"
            }
            tone={config?.sample_data_active ? "info" : config?.replay_caught_up ? "healthy" : "warning"}
          />
        </div>
      </div>

      {chartViewModel.error ? (
        <div className="section-block__empty live-chart__notice" role="status">
          {chartViewModel.error}
        </div>
      ) : null}

      {chartViewModel.historyError ? (
        <div className="section-block__empty live-chart__notice" role="status">
          {chartViewModel.historyError}
        </div>
      ) : null}

      {operationalAlerts.length ? (
        <div className="live-chart__alerts" aria-label="Chart operational alerts">
          {operationalAlerts.map((alert) => (
            <div key={alert.id} className={`banner banner--${alert.tone}`}>
              <strong>{alert.headline}</strong>
              <span title={alert.detail}>{alert.detail}</span>
            </div>
          ))}
        </div>
      ) : null}

      {!config?.available ? (
        <div className="live-chart__unavailable">
          <p>{config?.detail ?? "Load a strategy to chart the resolved contract."}</p>
        </div>
      ) : (
        <div className="live-chart">
          <section className="live-chart__stage">
            <div className="live-chart__frame">
              <div className="live-chart__readout-strip" aria-label="Chart operator readouts">
                <div className="live-chart__readout-card">
                  <span>Flow</span>
                  <strong>
                    {runtimeStatus
                      ? `${formatMode(runtimeStatus.mode)} / ${formatMode(runtimeStatus.arm_state)}`
                      : "Waiting for runtime"}
                  </strong>
                  <p>
                    {runtimeStatus?.command_dispatch_ready
                      ? "Dispatch ok"
                      : compactAlertDetail(
                          runtimeStatus?.command_dispatch_detail,
                          "Dispatch off",
                        )}
                  </p>
                </div>
                <div className="live-chart__readout-card">
                  <span>Candle</span>
                  <strong>
                    {latestBarSummary
                      ? `O ${formatDecimal(latestBarSummary.open)} | H ${formatDecimal(latestBarSummary.high)} | L ${formatDecimal(latestBarSummary.low)} | C ${formatDecimal(latestBarSummary.close)}`
                      : "Waiting for chart bars"}
                  </strong>
                  <p>{latestBarTimestampLabel}</p>
                </div>
                <div className="live-chart__readout-card">
                  <span>Pos</span>
                  <strong>{activePositionSummary(activePosition)}</strong>
                  <p>
                    {activePosition?.protective_orders_present
                      ? "Protections on"
                      : "Protections unconfirmed"}
                  </p>
                </div>
                <div className="live-chart__readout-card">
                  <span>Orders</span>
                  <strong>{workingOrderSummary(workingOrders)}</strong>
                  <p>
                    {`${formatInteger(recentFills.length)} fills | ${chartStreamLabel(chartViewModel.streamState)}`}
                  </p>
                </div>
              </div>
              {snapshot?.bars.length ? (
                <LiveChartCanvas
                  chartViewModel={chartViewModel}
                  fitRequestToken={fitRequestToken}
                  liveFollowEnabled={liveFollowEnabled}
                  onRequestOlderHistory={onLoadOlderHistory}
                />
              ) : (
                <div className="live-chart__empty">
                  <p>
                    Waiting for buffered candles from the local runtime host before drawing the
                    contract chart.
                  </p>
                </div>
              )}
            </div>
          </section>
        </div>
      )}
    </Panel>
  );
}
