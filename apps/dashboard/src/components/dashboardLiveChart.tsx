import { useEffect, useMemo, useRef, useState } from "react";
import {
  CandlestickSeries,
  ColorType,
  createChart,
  createSeriesMarkers,
  CrosshairMode,
  HistogramSeries,
  LineStyle,
  type IChartApi,
  type IPriceLine,
  type ISeriesApi,
  type ISeriesMarkersPluginApi,
  type Time,
} from "lightweight-charts";

import type { ChartViewModel } from "../dashboardModels";
import {
  chartPriceLines,
  chartTimeframeLabel,
  decimalToNumber,
  toChartCandles,
  toFillMarkers,
  toVolumeHistogram,
} from "../lib/chartAdapter";
import {
  formatCurrency,
  formatDateTime,
  formatDecimal,
  formatInteger,
  formatMode,
  formatSignedCurrency,
} from "../lib/format";
import type { RuntimeStatusSnapshot, Timeframe } from "../types/controlApi";
import {
  Definition,
  Metric,
  MiniMetric,
  Panel,
  Pill,
  SectionBlock,
} from "./dashboardPrimitives";

const CHART_HEIGHT = 420;
const CHART_INITIAL_FIT_TOKEN = 0;

interface ChartOperationalAlert {
  id: string;
  tone: "healthy" | "warning" | "danger" | "info";
  headline: string;
  detail: string;
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

function LiveChartCanvas({
  chartViewModel,
  fitRequestToken,
  liveFollowEnabled,
}: {
  chartViewModel: ChartViewModel;
  fitRequestToken: number;
  liveFollowEnabled: boolean;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<"Candlestick", Time> | null>(null);
  const volumeSeriesRef = useRef<ISeriesApi<"Histogram", Time> | null>(null);
  const fillMarkersRef = useRef<ISeriesMarkersPluginApi<Time> | null>(null);
  const priceLinesRef = useRef<IPriceLine[]>([]);
  const previousTimeframeRef = useRef<Timeframe | null>(null);
  const previousLatestClosedAtRef = useRef<string | null>(null);
  const previousBarCountRef = useRef(0);

  useEffect(() => {
    const container = containerRef.current;

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

    const volumeSeries = chart.addSeries(HistogramSeries, {
      priceFormat: {
        type: "volume",
      },
      priceScaleId: "",
      lastValueVisible: false,
      priceLineVisible: false,
    });
    volumeSeries.priceScale().applyOptions({
      scaleMargins: {
        top: 0.78,
        bottom: 0,
      },
      borderVisible: false,
    });

    const markers = createSeriesMarkers(candleSeries, [], {
      zOrder: "aboveSeries",
      autoScale: true,
    });

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;
    volumeSeriesRef.current = volumeSeries;
    fillMarkersRef.current = markers;

    let resizeFrame = 0;
    const resizeChart = () => {
      const nextWidth = Math.max(container.clientWidth, 320);
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

    return () => {
      if (resizeFrame !== 0) {
        window.cancelAnimationFrame(resizeFrame);
        resizeFrame = 0;
      }
      window.removeEventListener("resize", scheduleResize);

      chart.remove();
      chartRef.current = null;
      candleSeriesRef.current = null;
      volumeSeriesRef.current = null;
      fillMarkersRef.current = null;
      priceLinesRef.current = [];
    };
  }, []);

  useEffect(() => {
    chartRef.current?.applyOptions({
      timeScale: {
        secondsVisible: chartViewModel.selectedTimeframe === "1s",
      },
    });
  }, [chartViewModel.selectedTimeframe]);

  useEffect(() => {
    const chart = chartRef.current;
    const candleSeries = candleSeriesRef.current;
    const volumeSeries = volumeSeriesRef.current;

    if (!chart || !candleSeries || !volumeSeries) {
      return;
    }

    const snapshot = chartViewModel.snapshot;
    const candles = snapshot ? toChartCandles(snapshot.bars) : [];
    const volumes = snapshot ? toVolumeHistogram(snapshot.bars) : [];

    candleSeries.setData(candles);
    volumeSeries.setData(volumes);
    fillMarkersRef.current?.setMarkers(toFillMarkers(snapshot));

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

    if (candles.length > 0) {
      const timeframeChanged =
        previousTimeframeRef.current !== chartViewModel.selectedTimeframe;
      const firstRender = previousBarCountRef.current === 0;
      const latestClosedAt = snapshot?.latest_closed_at ?? null;
      const newTailBar =
        latestClosedAt !== null && latestClosedAt !== previousLatestClosedAtRef.current;

      if (timeframeChanged || firstRender) {
        chart.timeScale().fitContent();
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
  }, [chartViewModel.selectedTimeframe, chartViewModel.snapshot, liveFollowEnabled]);

  useEffect(() => {
    if (fitRequestToken === CHART_INITIAL_FIT_TOKEN) {
      return;
    }

    chartRef.current?.timeScale().fitContent();
  }, [fitRequestToken]);

  useEffect(() => {
    if (!liveFollowEnabled || !chartViewModel.snapshot?.bars.length) {
      return;
    }

    chartRef.current?.timeScale().scrollToRealTime();
  }, [chartViewModel.snapshot?.bars.length, liveFollowEnabled]);

  return <div ref={containerRef} className="live-chart__canvas" />;
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
    return "No active broker position";
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

  if (runtimeStatus?.reconnect_review.required) {
    alerts.push({
      id: "reconnect-review",
      tone: "warning",
      headline: "Reconnect review active for chart contract",
      detail:
        runtimeStatus.reconnect_review.reason ??
        `Operator review is required before normal execution resumes. Open positions: ${runtimeStatus.reconnect_review.open_position_count}; working orders: ${runtimeStatus.reconnect_review.working_order_count}.`,
    });
  }

  if (runtimeStatus?.shutdown_review.blocked) {
    alerts.push({
      id: "shutdown-review",
      tone: "warning",
      headline: "Shutdown review still blocking this contract",
      detail:
        runtimeStatus.shutdown_review.reason ??
        "Shutdown stays blocked until the operator resolves the open broker position state.",
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
      headline: "Chart feed degraded",
      detail:
        runtimeStatus?.market_data_detail ??
        marketData?.last_disconnect_reason ??
        "New entries stay blocked while the market-data session recovers; broker-protected positions remain untouched.",
    });
  }

  if (chartViewModel.streamState === "error" || chartViewModel.streamState === "closed") {
    alerts.push({
      id: "chart-stream",
      tone: chartViewModel.streamState === "error" ? "danger" : "warning",
      headline: "Chart stream reconnecting",
      detail:
        chartViewModel.error ??
        "The dedicated chart stream dropped and is reconnecting. Buffered history stays available and the chart can still be refreshed manually.",
    });
  } else if (chartViewModel.streamState === "connecting") {
    alerts.push({
      id: "chart-stream-connecting",
      tone: "info",
      headline: "Chart stream connecting",
      detail:
        "The dedicated chart stream is establishing. Snapshot data remains visible while live updates catch up.",
    });
  }

  if (runtimeStatus && !runtimeStatus.command_dispatch_ready) {
    alerts.push({
      id: "dispatch-unavailable",
      tone: "warning",
      headline: "Runtime dispatch unavailable from chart context",
      detail: runtimeStatus.command_dispatch_detail,
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
  const latestPrice = decimalToNumber(snapshot?.latest_price ?? null);
  const activePosition = snapshot?.active_position ?? null;
  const workingOrders = snapshot?.working_orders ?? [];
  const recentFills = snapshot?.recent_fills ?? [];
  const realizedPnl = decimalToNumber(activePosition?.realized_pnl ?? null);
  const unrealizedPnl = decimalToNumber(activePosition?.unrealized_pnl ?? null);
  const activePositionPrice = decimalToNumber(activePosition?.average_price ?? null);
  const latestClosedAt = snapshot?.latest_closed_at ?? null;

  const chartTitle = instrument
    ? `${instrument.strategy_name} live contract chart`
    : "Live contract chart";

  const latestBarSummary = useMemo(() => {
    if (!snapshot?.bars.length) {
      return null;
    }

    return snapshot.bars[snapshot.bars.length - 1];
  }, [snapshot]);
  const operationalAlerts = useMemo(
    () => chartOperationalAlerts(runtimeStatus, chartViewModel),
    [chartViewModel, runtimeStatus],
  );
  const [liveFollowEnabled, setLiveFollowEnabled] = useState(true);
  const [fitRequestToken, setFitRequestToken] = useState(CHART_INITIAL_FIT_TOKEN);

  return (
    <Panel
      className="panel--full panel--chart"
      eyebrow="Live chart"
      title={chartTitle}
      detail="Dedicated /chart snapshot and /chart/stream data for the currently loaded contract."
    >
      <div className="chart-toolbar">
        <div className="chart-toolbar__group">
          <Pill
            label={
              config?.available
                ? `${instrument?.tradovate_symbol ?? instrument?.canonical_symbol ?? "contract"}`
                : "Chart unavailable"
            }
            tone={config?.available ? "info" : "warning"}
          />
          <Pill
            label={`Stream ${chartViewModel.streamState}`}
            tone={chartStreamTone(chartViewModel.streamState)}
          />
          <Pill
            label={`Feed ${config?.market_data_health ?? "unknown"}`}
            tone={healthTone(config?.market_data_health ?? null)}
          />
          <Pill
            label={config?.trade_ready ? "Trade ready" : "Warmup in progress"}
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
            {liveFollowEnabled ? "Live follow on" : "Live follow off"}
          </button>
          <button
            className="command-button"
            type="button"
            onClick={() => {
              setFitRequestToken((current) => current + 1);
            }}
          >
            Fit chart
          </button>
          <button
            className="command-button"
            type="button"
            onClick={onRefreshChart}
          >
            Refresh chart
          </button>
          <button
            className="command-button"
            type="button"
            onClick={onLoadOlderHistory}
            disabled={
              !snapshot?.can_load_older_history || chartViewModel.historyState === "loading"
            }
          >
            {chartViewModel.historyState === "loading" ? "Loading older bars" : "Load older bars"}
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
        <p className="chart-toolbar__note">
          {chartViewModel.selectedTimeframe
            ? `Showing ${chartTimeframeLabel(chartViewModel.selectedTimeframe)} candles for the currently loaded strategy contract.`
            : config?.detail ?? "Load a strategy to chart its resolved contract."}
          {config?.available
            ? ` ${liveFollowEnabled ? "Live follow keeps the latest candle in view." : "Manual pan and zoom stay pinned until you refit or re-enable follow."}`
            : ""}
        </p>
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
              <span>{alert.detail}</span>
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
                  <span>Execution posture</span>
                  <strong>
                    {runtimeStatus
                      ? `${formatMode(runtimeStatus.mode)} | ${formatMode(runtimeStatus.arm_state)}`
                      : "Waiting for runtime"}
                  </strong>
                  <p>
                    {runtimeStatus?.command_dispatch_ready
                      ? runtimeStatus.command_dispatch_detail
                      : runtimeStatus?.command_dispatch_detail ?? "Dispatch unavailable"}
                  </p>
                </div>
                <div className="live-chart__readout-card">
                  <span>Latest candle</span>
                  <strong>
                    {latestBarSummary
                      ? `O ${formatDecimal(latestBarSummary.open)} | H ${formatDecimal(latestBarSummary.high)} | L ${formatDecimal(latestBarSummary.low)} | C ${formatDecimal(latestBarSummary.close)}`
                      : "Waiting for chart bars"}
                  </strong>
                  <p>{latestClosedAt ? formatDateTime(latestClosedAt) : "No candle closed yet"}</p>
                </div>
                <div className="live-chart__readout-card">
                  <span>Position context</span>
                  <strong>{activePositionSummary(activePosition)}</strong>
                  <p>
                    {activePosition?.protective_orders_present
                      ? "Broker protections are present on the active position."
                      : "Broker protections are not currently confirmed on the active position."}
                  </p>
                </div>
                <div className="live-chart__readout-card">
                  <span>Working order ladder</span>
                  <strong>{workingOrderSummary(workingOrders)}</strong>
                  <p>
                    {`${formatInteger(recentFills.length)} recent fill(s) | stream ${chartViewModel.streamState}`}
                  </p>
                </div>
              </div>
              {snapshot?.bars.length ? (
                <LiveChartCanvas
                  chartViewModel={chartViewModel}
                  fitRequestToken={fitRequestToken}
                  liveFollowEnabled={liveFollowEnabled}
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
            <div className="metric-row live-chart__metrics">
              <Metric
                label="Latest price"
                value={latestPrice === null ? "Unavailable" : formatDecimal(latestPrice)}
              />
              <Metric
                label="Buffered bars"
                value={formatInteger(snapshot?.bars.length ?? 0)}
              />
              <Metric
                label="Latest candle"
                value={latestClosedAt ? formatDateTime(latestClosedAt) : "Waiting"}
              />
              <Metric
                label="Warmup"
                value={
                  config
                    ? config.replay_caught_up
                      ? "Caught up"
                      : "Replaying"
                    : "Waiting"
                }
              />
            </div>
            <div className="subgrid">
              <MiniMetric
                label="Replay state"
                value={config?.replay_caught_up ? "Caught up" : "Building history"}
              />
              <MiniMetric
                label="Trade readiness"
                value={config?.trade_ready ? "Ready" : "Not ready"}
              />
              <MiniMetric
                label="Connection"
                value={config?.market_data_connection_state ?? "Unavailable"}
              />
              <MiniMetric
                label="Last stream update"
                value={formatDateTime(chartViewModel.lastStreamedAt)}
              />
            </div>
          </section>

          <aside className="live-chart__sidebar">
            <SectionBlock
              title="Contract context"
              note="The chart is locked to the contract resolved from the currently loaded strategy."
            >
              <dl className="definition-list">
                <Definition label="Strategy" value={instrument?.strategy_name ?? "Not loaded"} />
                <Definition
                  label="Tradovate symbol"
                  value={instrument?.tradovate_symbol ?? "Unavailable"}
                />
                <Definition
                  label="Canonical symbol"
                  value={instrument?.canonical_symbol ?? "Unavailable"}
                />
                <Definition
                  label="Databento mapping"
                  value={instrument?.databento_symbols.join(", ") || "Unavailable"}
                />
                <Definition label="Chart detail" value={config?.detail ?? "Unavailable"} />
                <Definition
                  label="Reconnect review"
                  value={
                    runtimeStatus?.reconnect_review.required
                      ? runtimeStatus.reconnect_review.reason ?? "Review required"
                      : "Clear"
                  }
                />
              </dl>
            </SectionBlock>

            <SectionBlock
              title="Active position"
              note="Position context comes directly from the synchronized broker snapshot."
            >
              {activePosition ? (
                <>
                  <div className="pill-row">
                    <Pill
                      label={activePosition.quantity >= 0 ? "Long position" : "Short position"}
                      tone="info"
                    />
                    <Pill
                      label={
                        activePosition.protective_orders_present
                          ? "Broker protections present"
                          : "Protections missing"
                      }
                      tone={activePosition.protective_orders_present ? "healthy" : "warning"}
                    />
                  </div>
                  <dl className="definition-list">
                    <Definition
                      label="Quantity"
                      value={formatInteger(activePosition.quantity)}
                    />
                    <Definition
                      label="Average price"
                      value={
                        activePositionPrice === null
                          ? "Unavailable"
                          : formatDecimal(activePositionPrice)
                      }
                    />
                    <Definition
                      label="Realized P&L"
                      value={
                        realizedPnl === null
                          ? "Unavailable"
                          : formatSignedCurrency(realizedPnl)
                      }
                    />
                    <Definition
                      label="Unrealized P&L"
                      value={
                        unrealizedPnl === null
                          ? "Unavailable"
                          : formatSignedCurrency(unrealizedPnl)
                      }
                    />
                    <Definition
                      label="Captured"
                      value={formatDateTime(activePosition.captured_at)}
                    />
                  </dl>
                </>
              ) : (
                <p className="section-block__empty">
                  No active position is currently projected for this chart contract.
                </p>
              )}
            </SectionBlock>

            <SectionBlock
              title="Working orders"
              note="Working orders are projected alongside the chart, and exact limit or stop levels are drawn when the broker snapshot exposes them."
            >
              {workingOrders.length ? (
                <ul className="event-list event-list--compact">
                  {workingOrders.map((order) => (
                    <li key={order.broker_order_id} className="event-list__item">
                      <div className="event-list__header">
                        <strong>{order.broker_order_id}</strong>
                        <Pill label={order.status} tone="warning" />
                      </div>
                      <p>
                        {`${order.side ?? "side?"} ${formatInteger(order.quantity)} | ${order.order_type ?? "type unavailable"} | ${workingOrderLevelsSummary(order)} | filled ${formatInteger(order.filled_quantity)} | avg fill ${formatDecimal(order.average_fill_price)}`}
                      </p>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="section-block__empty">
                  No working broker orders are projected for this contract right now.
                </p>
              )}
            </SectionBlock>

            <SectionBlock
              title="Recent fills"
              note="Recent fills are plotted on the chart with buy and sell markers."
            >
              {recentFills.length ? (
                <ul className="event-list event-list--compact">
                  {recentFills.map((fill) => (
                    <li key={fill.fill_id} className="event-list__item">
                      <div className="event-list__header">
                        <strong>{`${fill.side} ${formatInteger(fill.quantity)}`}</strong>
                        <Pill
                          label={formatDecimal(fill.price)}
                          tone={fill.side === "buy" ? "healthy" : "danger"}
                        />
                      </div>
                      <p>
                        {`Fill ${fill.fill_id}${fill.broker_order_id ? ` | order ${fill.broker_order_id}` : ""} | fee ${formatCurrency(fill.fee)} | commission ${formatCurrency(fill.commission)} | ${formatDateTime(fill.occurred_at)}`}
                      </p>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="section-block__empty">
                  No recent fills are projected for this contract yet.
                </p>
              )}
            </SectionBlock>

            <SectionBlock
              title="Latest candle detail"
              note="Current bar values from the local chart snapshot."
            >
              {latestBarSummary ? (
                <dl className="definition-list">
                  <Definition label="Open" value={formatDecimal(latestBarSummary.open)} />
                  <Definition label="High" value={formatDecimal(latestBarSummary.high)} />
                  <Definition label="Low" value={formatDecimal(latestBarSummary.low)} />
                  <Definition label="Close" value={formatDecimal(latestBarSummary.close)} />
                  <Definition
                    label="Volume"
                    value={formatInteger(latestBarSummary.volume)}
                  />
                  <Definition
                    label="Closed"
                    value={formatDateTime(latestBarSummary.closed_at)}
                  />
                </dl>
              ) : (
                <p className="section-block__empty">
                  Latest candle values will appear here once the runtime projects chart bars.
                </p>
              )}
            </SectionBlock>
          </aside>
        </div>
      )}
    </Panel>
  );
}
