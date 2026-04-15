import type { DashboardSnapshot } from "../lib/api";
import {
  formatCurrency,
  formatDateTime,
  formatDecimal,
  formatInteger,
  formatLatency,
  formatMode,
  formatSignedCurrency,
  formatWarmupMode,
} from "../lib/format";
import {
  formatDurationMinutes,
  formatPercentage,
  humanMemory,
  journalRecordTone,
  latestLatency,
  minutesBetween,
  prettyJson,
  reviewSummary,
  statusTone,
  tradeTone,
} from "../lib/dashboardPresentation";
import type {
  EventFeedViewModel,
  HeadlineSummary,
  JournalSummaryViewModel,
  LatencyStageViewModel,
  PerTradePnlViewModel,
  PnlChartViewModel,
  TradePerformanceViewModel,
} from "../dashboardModels";
import type {
  EventJournalRecord,
  FeedStatus,
  FillRecord,
  OrderRecord,
  PnlSnapshotRecord,
  TradeSummaryRecord,
} from "../types/controlApi";
import {
  Definition,
  Metric,
  MiniMetric,
  Panel,
  Pill,
  SectionBlock,
} from "./dashboardPrimitives";

export function RuntimeSummaryPanel({ snapshot }: { snapshot: DashboardSnapshot }) {
  return (
    <Panel
      eyebrow="Runtime"
      title={reviewSummary(snapshot.status)}
      detail={`HTTP ${snapshot.status.http_bind} | WS ${snapshot.status.websocket_bind}`}
    >
      <div className="metric-row">
        <Metric label="Arm state" value={formatMode(snapshot.status.arm_state)} />
        <Metric label="Warmup" value={formatMode(snapshot.status.warmup_status)} />
        <Metric label="Account" value={snapshot.status.current_account_name ?? "Not selected"} />
        <Metric
          label="Dispatch"
          value={snapshot.status.command_dispatch_ready ? "Ready" : "Blocked"}
        />
      </div>
      <div className="pill-row">
        <Pill label={formatMode(snapshot.status.mode)} tone="info" />
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
          tone={snapshot.status.operator_new_entries_enabled ? "healthy" : "warning"}
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
              : snapshot.status.operator_new_entries_reason ?? "Disabled by operator control"
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
  );
}

export function ReadinessPanel({
  snapshot,
  readinessCounts,
}: {
  snapshot: DashboardSnapshot;
  readinessCounts: { pass: number; warning: number; blocking: number };
}) {
  return (
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
  );
}

export function HealthPanel({
  snapshot,
  feedStatuses,
}: {
  snapshot: DashboardSnapshot;
  feedStatuses: FeedStatus[];
}) {
  return (
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
        <Definition label="Dispatch" value={snapshot.status.command_dispatch_detail} />
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
              value={snapshot.status.broker_status?.last_disconnect_reason ?? "No disconnect reason"}
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
              value={snapshot.status.market_data_status?.replay_caught_up ? "Caught up" : "Behind"}
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
  );
}

export function HistoryPanel({
  snapshot,
  openWorkingOrders,
  recentFills,
  recentTrades,
  tradePerformance,
  pnlChart,
  pnlChartPathData,
  perTradePnl,
  projectedPnlSnapshot,
}: {
  snapshot: DashboardSnapshot;
  openWorkingOrders: OrderRecord[];
  recentFills: FillRecord[];
  recentTrades: TradeSummaryRecord[];
  tradePerformance: TradePerformanceViewModel | null;
  pnlChart: PnlChartViewModel | null;
  pnlChartPathData: string;
  perTradePnl: PerTradePnlViewModel[];
  projectedPnlSnapshot: PnlSnapshotRecord | null;
}) {
  return (
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
        <Metric label="Win rate" value={formatPercentage(tradePerformance?.winRate)} />
        <Metric label="Avg net/trade" value={formatSignedCurrency(tradePerformance?.averageNet)} />
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
      <div className="section-grid section-grid--wide">
        <SectionBlock
          title="Real-time P&L chart"
          note="Projected from host-tracked trade summaries and the latest floating P&L snapshot."
          className="section-block--span-7"
        >
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
            <p className="section-block__empty">
              Waiting on enough projected trade history to draw the real-time P&amp;L chart.
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
            <MiniMetric label="Win rate" value={formatPercentage(tradePerformance?.winRate)} />
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
        </SectionBlock>
        <SectionBlock
          title="Per-trade P&L"
          note="Per-trade cards come directly from the host-projected trade summaries."
          className="section-block--span-5"
        >
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
            <p className="section-block__empty">
              Per-trade P&amp;L appears once projected trade summaries are available.
            </p>
          )}
        </SectionBlock>
      </div>
      <div className="section-grid section-grid--wide">
        <SectionBlock title="Open working orders" className="section-block--span-4">
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
            <p className="section-block__empty">No working broker orders are projected right now.</p>
          )}
        </SectionBlock>
        <SectionBlock title="Recent fills" className="section-block--span-4">
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
            <p className="section-block__empty">No broker fills have been recorded yet.</p>
          )}
        </SectionBlock>
        <SectionBlock title="Trade ledger" className="section-block--span-4">
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
            <p className="section-block__empty">No trade summaries are projected yet.</p>
          )}
        </SectionBlock>
      </div>
      {projectedPnlSnapshot ? (
        <p className="panel__footnote">
          Latest floating snapshot: {formatSignedCurrency(projectedPnlSnapshot.net_pnl)} net,{" "}
          {formatSignedCurrency(projectedPnlSnapshot.unrealized_pnl)} unrealized, captured{" "}
          {formatDateTime(projectedPnlSnapshot.captured_at)}.
        </p>
      ) : null}
    </Panel>
  );
}

export function LatencyPanel({
  snapshot,
  latencyBreakdown,
  slowestLatencyStage,
}: {
  snapshot: DashboardSnapshot;
  latencyBreakdown: LatencyStageViewModel[];
  slowestLatencyStage: LatencyStageViewModel | null;
}) {
  return (
    <Panel eyebrow="Latency" title="Latest trade-path timing">
      <div className="metric-row">
        <Metric
          label="Recorded paths"
          value={formatInteger(snapshot.status.recorded_trade_latency_count)}
        />
        <Metric label="End to end fill" value={formatLatency(latestLatency(snapshot.status))} />
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
      <div className="section-grid section-grid--wide">
        <SectionBlock
          title="Latency stage breakdown"
          note="Each stage bar is normalized against the slowest stage in the latest recorded path."
          className="section-block--span-7"
        >
          {latencyBreakdown.some((stage) => stage.value !== null) ? (
            <ul className="latency-list">
              {latencyBreakdown.map((stage) => (
                <li key={stage.key} className="latency-list__item">
                  <div className="latency-list__header">
                    <strong>{stage.label}</strong>
                    <span>{formatLatency(stage.value)}</span>
                  </div>
                  <div className="latency-list__track">
                    <span className="latency-list__bar" style={{ width: `${stage.barPercent}%` }} />
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            <p className="section-block__empty">
              Waiting for the runtime to publish its first trade-path latency record.
            </p>
          )}
        </SectionBlock>
        <SectionBlock
          title="Latency and host correlation"
          note="Correlates the latest trade path with host write lag and reconnect pressure."
          className="section-block--span-5"
        >
          <div className="subgrid">
            <MiniMetric
              label="Signal"
              value={formatLatency(snapshot.health.latest_trade_latency?.latency.signal_latency_ms)}
            />
            <MiniMetric
              label="Decision"
              value={formatLatency(
                snapshot.health.latest_trade_latency?.latency.decision_latency_ms,
              )}
            />
            <MiniMetric
              label="Order send"
              value={formatLatency(
                snapshot.health.latest_trade_latency?.latency.order_send_latency_ms,
              )}
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
        </SectionBlock>
      </div>
    </Panel>
  );
}

export function JournalPanel({
  snapshot,
  journalSummary,
  journalRecords,
}: {
  snapshot: DashboardSnapshot;
  journalSummary: JournalSummaryViewModel;
  journalRecords: EventJournalRecord[];
}) {
  return (
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
      <div className="section-grid section-grid--wide">
        <SectionBlock
          title="Journal summary"
          note="Top categories and source breakdown from the persisted event journal."
          className="section-block--span-4"
        >
          <div className="pill-row">
            <Pill label={`Dashboard ${formatInteger(journalSummary.dashboardCount)}`} tone="info" />
            <Pill label={`System ${formatInteger(journalSummary.systemCount)}`} tone="healthy" />
            <Pill label={`CLI ${formatInteger(journalSummary.cliCount)}`} tone="warning" />
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
          ) : (
            <p className="section-block__empty">No categorized journal activity yet.</p>
          )}
        </SectionBlock>
        <SectionBlock
          title="Recent audit records"
          note="Newest journal records first, including persisted payloads."
          className="section-block--span-8"
        >
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
            <p className="section-block__empty">
              No persisted journal records are available through the local runtime host yet.
            </p>
          )}
        </SectionBlock>
      </div>
    </Panel>
  );
}

export function EventsPanel({
  eventFeed,
  eventHeadlineSummary,
}: {
  eventFeed: EventFeedViewModel;
  eventHeadlineSummary: HeadlineSummary[];
}) {
  return (
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
      <div className="section-grid section-grid--wide">
        <SectionBlock
          title="Recent event mix"
          note="Summarizes the most frequent event headlines in the active stream window."
          className="section-block--span-4"
        >
          {eventHeadlineSummary.length ? (
            <div className="pill-row">
              {eventHeadlineSummary.map((entry) => (
                <Pill
                  key={entry.headline}
                  label={`${entry.headline} ${formatInteger(entry.count)}`}
                  tone={entry.tone}
                />
              ))}
            </div>
          ) : (
            <p className="section-block__empty">
              Waiting for the local event stream to publish recent activity.
            </p>
          )}
        </SectionBlock>
        <SectionBlock
          title="Recent operator feed"
          note="Newest events first from the local runtime event hub."
          className="section-block--span-8"
        >
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
            <p className="section-block__empty">
              The dashboard will render journal, readiness, command, health, and history updates
              here once the local event stream starts flowing.
            </p>
          )}
        </SectionBlock>
      </div>
    </Panel>
  );
}
