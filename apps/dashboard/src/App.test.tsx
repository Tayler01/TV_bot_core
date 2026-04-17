import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("lightweight-charts", () => {
  const createPriceLine = vi.fn(() => ({
    applyOptions: vi.fn(),
    options: vi.fn(() => ({})),
  }));
  const removePriceLine = vi.fn();
  const timeScale = {
    fitContent: vi.fn(),
    scrollToRealTime: vi.fn(),
    setVisibleLogicalRange: vi.fn(),
    getVisibleLogicalRange: vi.fn(() => ({ from: 0, to: 120 })),
    subscribeVisibleLogicalRangeChange: vi.fn(),
    unsubscribeVisibleLogicalRangeChange: vi.fn(),
  };
  const candleSeries = {
    setData: vi.fn(),
    createPriceLine,
    removePriceLine,
    priceLines: vi.fn(() => []),
    barsInLogicalRange: vi.fn(() => ({
      barsBefore: 120,
      barsAfter: 0,
      from: 0,
      to: 120,
    })),
  };
  const histogramSeries = {
    setData: vi.fn(),
    priceScale: vi.fn(() => ({
      applyOptions: vi.fn(),
    })),
  };

  return {
    createChart: vi.fn(() => ({
      addSeries: vi.fn((definition: { type?: string }) => {
        if (definition?.type === "Histogram") {
          return histogramSeries;
        }

        return candleSeries;
      }),
      applyOptions: vi.fn(),
      resize: vi.fn(),
      remove: vi.fn(),
      timeScale: vi.fn(() => timeScale),
    })),
    createSeriesMarkers: vi.fn(() => ({
      setMarkers: vi.fn(),
      markers: vi.fn(() => []),
      detach: vi.fn(),
    })),
    CandlestickSeries: { type: "Candlestick" },
    HistogramSeries: { type: "Histogram" },
    ColorType: {
      Solid: "solid",
    },
    CrosshairMode: {
      Normal: 0,
    },
    LineStyle: {
      Solid: 0,
      Dotted: 1,
      Dashed: 2,
    },
  };
});

import App from "./App";
import type {
  LoadedStrategySummary,
  RuntimeLifecycleCommand,
  RuntimeSettingsSnapshot,
  RuntimeStrategyValidationResponse,
} from "./types/controlApi";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json",
    },
  });
}

class MockWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: MockWebSocket[] = [];

  readonly url: string;
  readyState = MockWebSocket.CONNECTING;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onclose: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(url: string | URL) {
    this.url = String(url);
    MockWebSocket.instances.push(this);

    queueMicrotask(() => {
      if (this.readyState !== MockWebSocket.CONNECTING) {
        return;
      }

      this.readyState = MockWebSocket.OPEN;
      this.onopen?.(new Event("open"));
    });
  }

  close() {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.(new Event("close"));
  }

  emitJson(payload: unknown) {
    this.onmessage?.(
      new MessageEvent("message", {
        data: JSON.stringify(payload),
      }),
    );
  }
}

function installWebSocketMock() {
  MockWebSocket.instances = [];
  vi.stubGlobal("WebSocket", MockWebSocket as unknown as typeof WebSocket);

  return {
    latest(path = "/events") {
      return (
        [...MockWebSocket.instances].reverse().find((instance) => instance.url.includes(path)) ??
        MockWebSocket.instances.at(-1) ??
        null
      );
    },
  };
}

function installFetchMock(snapshotOverrides?: {
  reconnectRequired?: boolean;
  shutdownBlocked?: boolean;
  marketDataHealth?: "healthy" | "degraded" | "failed";
  sampleDataActive?: boolean;
}) {
  const reconnectRequired = snapshotOverrides?.reconnectRequired ?? false;
  const shutdownBlocked = snapshotOverrides?.shutdownBlocked ?? false;
  const marketDataHealth = snapshotOverrides?.marketDataHealth ?? "healthy";
  const sampleDataActive = snapshotOverrides?.sampleDataActive ?? false;
  const currentStrategy: LoadedStrategySummary = {
    path: "strategies/sample.md",
    title: "Sample",
    strategy_id: "gold-breakout",
    name: "Gold Breakout",
    version: "1.0.0",
    market_family: "metals",
    warning_count: 0,
  };

  const status = {
    mode: "paper",
    arm_state: "armed",
    warmup_status: "ready",
    strategy_loaded: true,
    hard_override_active: false,
    operator_new_entries_enabled: true,
    operator_new_entries_reason: null as string | null,
    current_strategy: currentStrategy,
    broker_status: {
      provider: "tradovate",
      environment: "demo",
      connection_state: "connected",
      health: "healthy",
      sync_state: "synchronized",
      selected_account: {
        provider: "tradovate",
        account_id: "paper-primary-id",
        account_name: "paper-primary",
        routing: "paper",
        environment: "demo",
        selected_at: "2026-04-12T20:10:00Z",
      },
      reconnect_count: 1,
      last_authenticated_at: "2026-04-12T20:10:00Z",
      last_heartbeat_at: "2026-04-12T20:11:00Z",
      last_sync_at: "2026-04-12T20:11:01Z",
      last_disconnect_reason: null,
      review_required_reason: reconnectRequired
        ? "existing broker-side position or working orders detected after reconnect"
        : null,
      updated_at: "2026-04-12T20:11:01Z",
    },
    market_data_status: {
      session: {
        market_data: {
          provider: "databento",
          dataset: "GLBX.MDP3",
          connection_state: marketDataHealth === "healthy" ? "connected" : "degraded",
          health: marketDataHealth,
          feed_statuses: [
            {
              instrument_symbol: "GCM2026",
              feed: "ohlcv_1m",
              state: marketDataHealth === "healthy" ? "ready" : "degraded",
              last_event_at: "2026-04-12T20:11:00Z",
              detail:
                marketDataHealth === "healthy"
                  ? "feed ready"
                  : "heartbeat stale; waiting for recovery",
            },
          ],
          warmup: {
            status: "ready",
            ready_requires_all: true,
            buffers: [],
            started_at: "2026-04-12T20:08:00Z",
            updated_at: "2026-04-12T20:11:00Z",
            failure_reason: null,
          },
          reconnect_count: 0,
          last_heartbeat_at: "2026-04-12T20:11:00Z",
          last_disconnect_reason:
            marketDataHealth === "healthy" ? null : "Databento heartbeat stale",
          updated_at: "2026-04-12T20:11:00Z",
        },
      },
      warmup_requested: true,
      warmup_mode: {
        ReplayFrom: "2026-04-12T18:50:00Z",
      },
      replay_caught_up: true,
      trade_ready: true,
      updated_at: "2026-04-12T20:11:00Z",
    },
    market_data_detail:
      marketDataHealth === "healthy"
        ? null
        : "Databento heartbeat stale; new entries paused until feed recovery.",
    storage_status: {
      mode: "primary_configured",
      primary_configured: true,
      sqlite_fallback_enabled: true,
      sqlite_path: "data/tv_bot_core.sqlite",
      allow_runtime_fallback: true,
      active_backend: "postgres",
      durable: true,
      fallback_activated: false,
      detail: "primary Postgres persistence active",
    },
    journal_status: {
      backend: "postgres",
      durable: true,
      detail: "persistent journal active",
    },
    system_health: {
      cpu_percent: 8.3,
      memory_bytes: 2147483648,
      reconnect_count: 1,
      db_write_latency_ms: 14,
      queue_lag_ms: 2,
      error_count: 0,
      feed_degraded: marketDataHealth !== "healthy",
      updated_at: "2026-04-12T20:11:03Z",
    },
    latest_trade_latency: {
      action_id: "manual-entry-1",
      strategy_id: "gold-breakout",
      recorded_at: "2026-04-12T20:11:03Z",
      latency: {
        signal_latency_ms: null,
        decision_latency_ms: 3,
        order_send_latency_ms: 11,
        broker_ack_latency_ms: 28,
        fill_latency_ms: 97,
        sync_update_latency_ms: 141,
        end_to_end_fill_latency_ms: 97,
      },
    },
    recorded_trade_latency_count: 4,
    current_account_name: "paper-primary",
    instrument_mapping: {
      market_family: "metals",
      market_display_name: "Gold",
      contract_mode: "front_month_auto",
      tradovate_symbol: "GCM2026",
      summary: "Gold front month resolved to GCM2026",
    },
    instrument_resolution_error: null,
    reconnect_review: {
      required: reconnectRequired,
      reason: reconnectRequired
        ? "existing broker-side position or working orders detected after reconnect"
        : null,
      last_decision: reconnectRequired ? null : "reattach_bot_management",
      open_position_count: reconnectRequired ? 1 : 0,
      working_order_count: reconnectRequired ? 1 : 0,
    },
    shutdown_review: {
      pending_signal: shutdownBlocked,
      blocked: shutdownBlocked,
      awaiting_flatten: false,
      decision: null,
      reason: shutdownBlocked ? "shutdown blocked until open position is resolved" : null,
      open_position_count: shutdownBlocked ? 1 : 0,
      all_positions_broker_protected: true,
    },
    http_bind: "127.0.0.1:8080",
    websocket_bind: "127.0.0.1:8081",
    command_dispatch_ready: true,
    command_dispatch_detail: "runtime command dispatcher ready",
  };

  const readiness = {
    status,
    report: {
      mode: "paper",
      checks: [
        {
          name: "mode",
          status: "pass",
          message: "paper mode selected",
        },
        {
          name: "broker health",
          status: "pass",
          message: "Tradovate paper session synchronized",
        },
        {
          name: "market data",
          status: "warning",
          message: "Warmup recently completed; continue watching feed heartbeat.",
        },
      ],
      risk_summary: "Protective brackets available and operator overrides are clear.",
      hard_override_required: false,
      generated_at: "2026-04-12T20:11:03Z",
    },
  };

  const history: any = {
    projection: {
      total_strategy_run_records: 1,
      total_order_records: 4,
      total_fill_records: 2,
      total_position_records: 2,
      total_pnl_snapshot_records: 2,
      total_trade_summary_records: 1,
      active_run_ids: ["run-1"],
      orders: {
        "8102": {
          broker_order_id: "8102",
          strategy_id: "gold-breakout",
          run_id: "run-1",
          account_id: "paper-primary-id",
          symbol: "GCM2026",
          side: "buy",
          order_type: "limit",
          quantity: 1,
          filled_quantity: 0,
          average_fill_price: null,
          status: "working",
          provider: "tradovate",
          submitted_at: "2026-04-12T20:10:55Z",
          updated_at: "2026-04-12T20:11:03Z",
        },
      },
      working_order_ids: ["8102"],
      fills: {
        "fill-1": {
          fill_id: "fill-1",
          broker_order_id: "8102",
          strategy_id: "gold-breakout",
          run_id: "run-1",
          account_id: "paper-primary-id",
          symbol: "GCM2026",
          side: "buy",
          quantity: 1,
          price: "2410.50",
          fee: "1.25",
          commission: "0.75",
          occurred_at: "2026-04-12T20:11:03Z",
        },
      },
      trade_summaries: {
        "trade-1": {
          trade_id: "trade-1",
          strategy_id: "gold-breakout",
          run_id: "run-1",
          account_id: "paper-primary-id",
          symbol: "GCM2026",
          side: "buy",
          status: "closed",
          quantity: 1,
          average_entry_price: "2406.00",
          average_exit_price: "2418.50",
          opened_at: "2026-04-12T19:58:00Z",
          closed_at: "2026-04-12T20:03:00Z",
          gross_pnl: "125.00",
          net_pnl: "118.00",
          fees: "3.00",
          commissions: "2.00",
          slippage: "2.00",
        },
      },
      open_position_symbols: ["GCM2026"],
      open_trade_ids: ["trade-1"],
      latest_order: {
        broker_order_id: "8102",
        strategy_id: "gold-breakout",
        run_id: "run-1",
        account_id: "paper-primary-id",
        symbol: "GCM2026",
        side: "buy",
        order_type: "limit",
        quantity: 1,
        filled_quantity: 0,
        average_fill_price: null,
        status: "working",
        provider: "tradovate",
        submitted_at: "2026-04-12T20:10:55Z",
        updated_at: "2026-04-12T20:11:03Z",
      },
      latest_fill: {
        fill_id: "fill-1",
        broker_order_id: "8102",
        strategy_id: "gold-breakout",
        run_id: "run-1",
        account_id: "paper-primary-id",
        symbol: "GCM2026",
        side: "buy",
        quantity: 1,
        price: "2410.50",
        fee: "1.25",
        commission: "0.75",
        occurred_at: "2026-04-12T20:11:03Z",
      },
      latest_position: {
        record_id: "position-1",
        strategy_id: "gold-breakout",
        run_id: "run-1",
        account_id: "paper-primary-id",
        symbol: "GCM2026",
        quantity: 1,
        average_price: "2410.50",
        realized_pnl: "0.00",
        unrealized_pnl: "45.25",
        protective_orders_present: true,
        captured_at: "2026-04-12T20:11:03Z",
      },
      latest_pnl_snapshot: {
        snapshot_id: "pnl-1",
        strategy_id: "gold-breakout",
        run_id: "run-1",
        account_id: "paper-primary-id",
        symbol: "GCM2026",
        gross_pnl: "52.10",
        net_pnl: "48.60",
        fees: "1.00",
        commissions: "1.50",
        slippage: "1.00",
        realized_pnl: "0.00",
        unrealized_pnl: "48.60",
        captured_at: "2026-04-12T20:11:03Z",
      },
      latest_trade_summary: {
        trade_id: "trade-1",
        strategy_id: "gold-breakout",
        run_id: "run-1",
        account_id: "paper-primary-id",
        symbol: "GCM2026",
        side: "buy",
        status: "closed",
        quantity: 1,
        average_entry_price: "2406.00",
        average_exit_price: "2418.50",
        opened_at: "2026-04-12T19:58:00Z",
        closed_at: "2026-04-12T20:03:00Z",
        gross_pnl: "125.00",
        net_pnl: "118.00",
        fees: "3.00",
        commissions: "2.00",
        slippage: "2.00",
      },
      closed_trade_count: 3,
      cancelled_trade_count: 0,
      closed_trade_gross_pnl: "110.00",
      closed_trade_net_pnl: "97.00",
      closed_trade_fees: "5.00",
      closed_trade_commissions: "4.00",
      closed_trade_slippage: "4.00",
      recorded_fill_fees: "5.00",
      recorded_fill_commissions: "4.00",
      last_activity_at: "2026-04-12T20:11:03Z",
    },
  };

  const journal: any = {
    total_records: 3,
    records: [
      {
        event_id: "evt-3",
        category: "execution",
        action: "dispatch_succeeded",
        source: "dashboard",
        severity: "info",
        occurred_at: "2026-04-12T20:11:04Z",
        payload: {
          broker_order_id: "8102",
          symbol: "GCM2026",
        },
      },
      {
        event_id: "evt-2",
        category: "risk",
        action: "decision",
        source: "system",
        severity: "info",
        occurred_at: "2026-04-12T20:11:03Z",
        payload: {
          status: "accepted",
          reason: "risk checks passed",
        },
      },
      {
        event_id: "evt-1",
        category: "execution",
        action: "intent_received",
        source: "dashboard",
        severity: "info",
        occurred_at: "2026-04-12T20:11:02Z",
        payload: {
          kind: "manual_entry",
        },
      },
    ],
  };

  const health = {
    status: "healthy",
    system_health: status.system_health,
    latest_trade_latency: status.latest_trade_latency,
  };

  const settings: RuntimeSettingsSnapshot = {
    editable: {
      startup_mode: "observation",
      default_strategy_path: "strategies/sample.md",
      allow_sqlite_fallback: true,
      paper_account_name: "paper-primary",
      live_account_name: "live-primary",
    },
    http_bind: "127.0.0.1:8080",
    websocket_bind: "127.0.0.1:8081",
    config_file_path: "runtime.example.toml",
    persistence_mode: "config_file",
    restart_required: true,
    detail:
      "settings edits are saved to the runtime config file for the next restart; environment overrides may still take precedence",
  };

  const chartConfig = {
    available: true,
    detail: sampleDataActive
      ? "missing market-data configuration: market_data.api_key; showing illustrative sample candles until live market data is configured"
      : "charting GCM2026 from the loaded strategy contract",
    sample_data_active: sampleDataActive,
    instrument: {
      strategy_id: "gold-breakout",
      strategy_name: "Gold Breakout",
      market_family: "metals",
      market_display_name: "Gold",
      tradovate_symbol: "GCM2026",
      canonical_symbol: "GCM6",
      databento_symbols: ["GCM6"],
      summary: "Gold front month resolved to GCM2026 / GCM6",
    },
    supported_timeframes: ["1s", "1m", "5m"],
    default_timeframe: "1m",
    market_data_connection_state:
      marketDataHealth === "healthy" ? "subscribed" : "degraded",
    market_data_health: marketDataHealth,
    replay_caught_up: true,
    trade_ready: true,
  };

  const chartBarsByTimeframe = {
    "1s": [
      {
        timeframe: "1s",
        open: "2411.80",
        high: "2412.10",
        low: "2411.60",
        close: "2411.95",
        volume: 18,
        closed_at: "2026-04-12T20:10:58Z",
        is_complete: true,
      },
      {
        timeframe: "1s",
        open: "2411.95",
        high: "2412.30",
        low: "2411.90",
        close: "2412.20",
        volume: 24,
        closed_at: "2026-04-12T20:10:59Z",
        is_complete: true,
      },
      {
        timeframe: "1s",
        open: "2412.20",
        high: "2412.35",
        low: "2412.00",
        close: "2412.25",
        volume: 16,
        closed_at: "2026-04-12T20:11:00Z",
        is_complete: true,
      },
    ],
    "1m": [
      {
        timeframe: "1m",
        open: "2408.40",
        high: "2410.10",
        low: "2407.80",
        close: "2409.60",
        volume: 140,
        closed_at: "2026-04-12T20:07:00Z",
        is_complete: true,
      },
      {
        timeframe: "1m",
        open: "2409.60",
        high: "2411.20",
        low: "2409.10",
        close: "2410.80",
        volume: 182,
        closed_at: "2026-04-12T20:08:00Z",
        is_complete: true,
      },
      {
        timeframe: "1m",
        open: "2410.80",
        high: "2412.70",
        low: "2410.20",
        close: "2411.90",
        volume: 210,
        closed_at: "2026-04-12T20:09:00Z",
        is_complete: true,
      },
      {
        timeframe: "1m",
        open: "2411.90",
        high: "2413.10",
        low: "2411.40",
        close: "2412.40",
        volume: 236,
        closed_at: "2026-04-12T20:10:00Z",
        is_complete: true,
      },
      {
        timeframe: "1m",
        open: "2412.40",
        high: "2412.90",
        low: "2411.70",
        close: "2412.25",
        volume: 148,
        closed_at: "2026-04-12T20:11:00Z",
        is_complete: false,
      },
    ],
    "5m": [
      {
        timeframe: "5m",
        open: "2404.10",
        high: "2410.80",
        low: "2403.60",
        close: "2409.50",
        volume: 620,
        closed_at: "2026-04-12T20:00:00Z",
        is_complete: true,
      },
      {
        timeframe: "5m",
        open: "2409.50",
        high: "2413.10",
        low: "2408.90",
        close: "2412.25",
        volume: 712,
        closed_at: "2026-04-12T20:05:00Z",
        is_complete: true,
      },
    ],
  } as const;

  const strategyLibrary = {
    scanned_roots: ["strategies/uploads", "strategies/examples"],
    strategies: [
      {
        path: "strategies/sample.md",
        display_path: "strategies/sample.md",
        valid: true,
        title: "Sample",
        strategy_id: "gold-breakout",
        name: "Gold Breakout",
        version: "1.0.0",
        market_family: "metals",
        warning_count: 0,
        error_count: 0,
      },
      {
        path: "strategies/broken.md",
        display_path: "strategies/broken.md",
        valid: false,
        title: "Broken Strategy",
        strategy_id: null,
        name: null,
        version: null,
        market_family: null,
        warning_count: 0,
        error_count: 2,
      },
    ],
  };
  let uploadedStrategyPath: string | null = null;
  let uploadedStrategyValidation: RuntimeStrategyValidationResponse | null = null;

  function validationForPath(path: string) {
    if (uploadedStrategyPath && path === uploadedStrategyPath && uploadedStrategyValidation) {
      return uploadedStrategyValidation;
    }

    if (path === "strategies/broken.md") {
      return {
        path,
        display_path: path,
        valid: false,
        title: "Broken Strategy",
        summary: null,
        warnings: [],
        errors: [
          {
            severity: "error",
            message: "Missing required `Session` section.",
            section: "Session",
            field: null,
            line: 1,
          },
          {
            severity: "error",
            message: "Missing required `Risk` section.",
            section: "Risk",
            field: null,
            line: 1,
          },
        ],
      };
    }

    return {
      path,
      display_path: path,
      valid: true,
      title: "Sample",
      summary: {
        path,
        title: "Sample",
        strategy_id: "gold-breakout",
        name: "Gold Breakout",
        version: "1.0.0",
        market_family: "metals",
        warning_count: 0,
      },
      warnings: [],
      errors: [],
    };
  }

  const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const endpoint =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.pathname
          : input.url;
    const parsedEndpoint = new URL(endpoint, "http://dashboard.test");
    const chartTimeframe =
      (parsedEndpoint.searchParams.get("timeframe") as "1s" | "1m" | "5m" | null) ?? "1m";
    const chartLimit = Number.parseInt(parsedEndpoint.searchParams.get("limit") ?? "240", 10);
    const chartBars = chartBarsByTimeframe[chartTimeframe];

    if (endpoint.endsWith("/status")) {
      return jsonResponse(status);
    }

    if (endpoint.endsWith("/readiness")) {
      return jsonResponse(readiness);
    }

    if (endpoint.endsWith("/history")) {
      return jsonResponse(history);
    }

    if (endpoint.endsWith("/journal")) {
      return jsonResponse(journal);
    }

    if (endpoint.endsWith("/health")) {
      return jsonResponse(health);
    }

    if (endpoint.endsWith("/settings") && (init?.method === undefined || init.method === "GET")) {
      return jsonResponse(settings);
    }

    if (endpoint.endsWith("/settings") && init?.method === "POST") {
      const request = JSON.parse(String(init.body ?? "{}")) as {
        source: string;
        settings: {
          startup_mode: RuntimeSettingsSnapshot["editable"]["startup_mode"];
          default_strategy_path: string | null;
          allow_sqlite_fallback: boolean;
          paper_account_name: string | null;
          live_account_name: string | null;
        };
      };

      settings.editable = {
        startup_mode: request.settings.startup_mode,
        default_strategy_path: request.settings.default_strategy_path,
        allow_sqlite_fallback: request.settings.allow_sqlite_fallback,
        paper_account_name: request.settings.paper_account_name,
        live_account_name: request.settings.live_account_name,
      };

      return jsonResponse({
        message: "saved runtime settings for the next restart",
        settings,
      });
    }

    if (parsedEndpoint.pathname === "/chart/config") {
      return jsonResponse(chartConfig);
    }

    if (parsedEndpoint.pathname === "/chart/snapshot") {
      return jsonResponse({
        config: chartConfig,
        timeframe: chartTimeframe,
        requested_limit: chartLimit,
        bars: chartBars.slice(Math.max(chartBars.length - chartLimit, 0)),
        latest_price: chartBars.at(-1)?.close ?? null,
        latest_closed_at: chartBars.at(-1)?.closed_at ?? null,
        active_position: {
          account_id: "paper-primary-id",
          symbol: "GCM2026",
          quantity: 1,
          average_price: "2410.50",
          realized_pnl: "0.00",
          unrealized_pnl: "48.60",
          protective_orders_present: true,
          captured_at: "2026-04-12T20:11:03Z",
        },
          working_orders: [
            {
              broker_order_id: "8102",
              account_id: "paper-primary-id",
              symbol: "GCM2026",
              side: "buy",
              quantity: 1,
              order_type: "limit",
              status: "working",
              filled_quantity: 0,
              limit_price: "2412.25",
              stop_price: "2408.75",
              average_fill_price: null,
              updated_at: "2026-04-12T20:11:03Z",
            },
          ],
        recent_fills: [
          {
            fill_id: "fill-1",
            broker_order_id: "8102",
            account_id: "paper-primary-id",
            symbol: "GCM2026",
            side: "buy",
            quantity: 1,
            price: "2410.50",
            fee: "1.25",
            commission: "0.75",
            occurred_at: "2026-04-12T20:11:03Z",
          },
        ],
        can_load_older_history: chartTimeframe !== "1s",
      });
    }

    if (parsedEndpoint.pathname === "/chart/history") {
      const before = parsedEndpoint.searchParams.get("before");
      const olderBars = before
        ? chartBars.filter((bar) => bar.closed_at < before)
        : chartBars;
      const pagedBars = olderBars.slice(Math.max(olderBars.length - chartLimit, 0));

      return jsonResponse({
        config: chartConfig,
        timeframe: chartTimeframe,
        requested_limit: chartLimit,
        before,
        bars: pagedBars,
        can_load_older_history: false,
      });
    }

    if (endpoint.endsWith("/strategies")) {
      return jsonResponse(strategyLibrary);
    }

    if (endpoint.endsWith("/strategies/upload")) {
      const request = JSON.parse(String(init?.body ?? "{}")) as {
        source: string;
        filename: string;
        markdown: string;
      };
      uploadedStrategyPath = `strategies/uploads/${request.filename}`;
      uploadedStrategyValidation = {
        path: uploadedStrategyPath,
        display_path: uploadedStrategyPath,
        valid: true,
        title: "Uploaded Breakout",
        summary: {
          path: uploadedStrategyPath,
          title: "Uploaded Breakout",
          strategy_id: "uploaded-breakout",
          name: "Uploaded Breakout",
          version: "1.0.0",
          market_family: "metals",
          warning_count: 0,
        },
        warnings: [],
        errors: [],
      };
      strategyLibrary.strategies = [
        {
          path: uploadedStrategyPath,
          display_path: uploadedStrategyPath,
          valid: true,
          title: "Uploaded Breakout",
          strategy_id: "uploaded-breakout",
          name: "Uploaded Breakout",
          version: "1.0.0",
          market_family: "metals",
          warning_count: 0,
          error_count: 0,
        },
        ...strategyLibrary.strategies,
      ];

      return jsonResponse(uploadedStrategyValidation);
    }

    if (endpoint.endsWith("/strategies/validate")) {
      const request = JSON.parse(String(init?.body ?? "{}")) as {
        source: string;
        path: string;
      };

      return jsonResponse(validationForPath(request.path));
    }

    if (endpoint.endsWith("/runtime/commands")) {
      const request = JSON.parse(String(init?.body ?? "{}")) as {
        source: string;
        command: {
          kind: string;
          mode?: string;
          allow_override?: boolean;
          contract_id?: number;
          reason?: string;
          path?: string;
          decision?: string;
          side?: "buy" | "sell";
          quantity?: number;
          tick_size?: string;
          entry_reference_price?: string;
          tick_value_usd?: string | null;
        };
      };

      switch (request.command.kind) {
        case "pause":
          status.mode = "paused";
          readiness.status.mode = "paused";
          readiness.report.mode = "paused";
          return jsonResponse(
            {
              status_code: "Ok",
              message: "runtime paused",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "resume":
          status.mode = "paper";
          readiness.status.mode = "paper";
          readiness.report.mode = "paper";
          return jsonResponse(
            {
              status_code: "Ok",
              message: "runtime resumed",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "set_mode":
          status.mode = request.command.mode ?? "paper";
          readiness.status.mode = status.mode;
          readiness.report.mode = status.mode;
          return jsonResponse(
            {
              status_code: "Ok",
              message: `runtime mode set to ${status.mode}`,
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "load_strategy": {
          const nextStrategy =
            validationForPath(request.command.path ?? "strategies/sample.md").summary ??
            status.current_strategy;
          status.strategy_loaded = true;
          status.current_strategy = nextStrategy;
          readiness.status.strategy_loaded = true;
          readiness.status.current_strategy = nextStrategy;

          return jsonResponse(
            {
              status_code: "Ok",
              message: `loaded strategy \`${nextStrategy?.strategy_id}\` from \`${request.command.path}\``,
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        }
        case "start_warmup":
          status.warmup_status = "warming";
          readiness.status.warmup_status = "warming";
          return jsonResponse(
            {
              status_code: "Ok",
              message: "warmup started",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "arm":
          status.arm_state = "armed";
          status.hard_override_active = Boolean(request.command.allow_override);
          readiness.status.arm_state = "armed";
          return jsonResponse(
            {
              status_code: "Ok",
              message: request.command.allow_override
                ? "runtime armed with temporary override"
                : "runtime armed",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "disarm":
          status.arm_state = "disarmed";
          status.hard_override_active = false;
          readiness.status.arm_state = "disarmed";
          return jsonResponse(
            {
              status_code: "Ok",
              message: "runtime disarmed",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "set_new_entries_enabled": {
          const command = request.command as Extract<
            RuntimeLifecycleCommand,
            { kind: "set_new_entries_enabled" }
          >;
          status.operator_new_entries_enabled = command.enabled;
          status.operator_new_entries_reason =
            command.enabled === false ? command.reason ?? null : null;
          readiness.status.operator_new_entries_enabled = status.operator_new_entries_enabled;
          readiness.status.operator_new_entries_reason = status.operator_new_entries_reason;
          readiness.report.checks = readiness.report.checks.filter(
            (check) => check.name !== "operator_entry_gate",
          );
          if (status.operator_new_entries_enabled === false) {
            readiness.report.checks.push({
              name: "operator_entry_gate",
              status: "warning",
              message: status.operator_new_entries_reason
                ? `new entries are disabled by operator control: ${status.operator_new_entries_reason}`
                : "new entries are disabled by operator control",
            });
          }
          return jsonResponse(
            {
              status_code: "Ok",
              message:
                status.operator_new_entries_enabled === false
                  ? `new entries disabled: ${status.operator_new_entries_reason ?? "dashboard operator entry gate"}`
                  : "new entries enabled",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        }
        case "resolve_reconnect_review":
          status.reconnect_review.required = false;
          status.reconnect_review.last_decision = request.command.decision ?? null;
          status.reconnect_review.reason = null;
          status.reconnect_review.open_position_count = 0;
          status.reconnect_review.working_order_count = 0;
          readiness.status.reconnect_review = status.reconnect_review;
          return jsonResponse(
            {
              status_code: "Ok",
              message: `reconnect review resolved with ${request.command.decision}`,
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "shutdown":
          if (request.command.decision === "flatten_first") {
            status.shutdown_review.blocked = true;
            status.shutdown_review.awaiting_flatten = true;
            (
              status.shutdown_review as { decision: string | null }
            ).decision = "flatten_first";
            status.shutdown_review.reason =
              "shutdown is waiting for flatten confirmation on 1 open position(s)";
            readiness.status.shutdown_review = status.shutdown_review;
            return jsonResponse(
              {
                status_code: "Ok",
                message: "shutdown will continue after the broker position is flat",
                status,
                readiness,
                command_result: null,
              },
              200,
            );
          }

          status.shutdown_review.blocked = false;
          status.shutdown_review.awaiting_flatten = false;
          (
            status.shutdown_review as { decision: string | null }
          ).decision = "leave_broker_protected";
          status.shutdown_review.reason = null;
          readiness.status.shutdown_review = status.shutdown_review;
          return jsonResponse(
            {
              status_code: "Ok",
              message:
                "shutdown approved; leaving 1 broker-protected open position(s) in place",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "close_position":
          history.projection.open_position_symbols = [];
          status.shutdown_review.open_position_count = 0;
          status.reconnect_review.open_position_count = 0;
          return jsonResponse(
            {
              status_code: "Ok",
              message: "close position command dispatched",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "manual_entry": {
          history.projection.total_order_records += 1;
          history.projection.orders["8110"] = {
            broker_order_id: "8110",
            strategy_id: "gold-breakout",
            run_id: "run-1",
            account_id: "paper-primary-id",
            symbol: "GCM2026",
            side: request.command.side ?? "buy",
            order_type: "market",
            quantity: request.command.quantity ?? 1,
            filled_quantity: 0,
            average_fill_price: null,
            status: "working",
            provider: "tradovate",
            submitted_at: "2026-04-12T20:12:00Z",
            updated_at: "2026-04-12T20:12:00Z",
          };
          history.projection.working_order_ids = ["8110", ...history.projection.working_order_ids];
          history.projection.latest_order = history.projection.orders["8110"];
          journal.total_records += 1;
          journal.records = [
            {
              event_id: "evt-4",
              category: "execution",
              action: "dispatch_succeeded",
              source: "dashboard",
              severity: "info",
              occurred_at: "2026-04-12T20:12:00Z",
              payload: {
                kind: "manual_entry",
                symbol: "GCM2026",
                broker_order_id: "8110",
              },
            },
            ...journal.records,
          ];
          return jsonResponse(
            {
              status_code: "Ok",
              message: "manual entry command dispatched",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        }
        case "cancel_working_orders":
          history.projection.working_order_ids = [];
          (
            history.projection.orders["8102"] as { status: string }
          ).status = "cancelled";
          return jsonResponse(
            {
              status_code: "Ok",
              message: "working-order cancellation dispatched",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        case "flatten":
          return jsonResponse(
            {
              status_code: "Ok",
              message: "flatten command dispatched",
              status,
              readiness,
              command_result: null,
            },
            200,
          );
        default:
          return jsonResponse(
            {
              status_code: "Conflict",
              message: `unhandled command ${request.command.kind}`,
              status,
              readiness,
              command_result: null,
            },
            409,
          );
      }
    }

    return new Response("not found", { status: 404 });
  });

  return {
    fetchSpy,
    status,
    readiness,
  };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("App", () => {
  it("renders the paper overview from the local control API snapshot", async () => {
    installWebSocketMock();
    installFetchMock();

    render(<App />);

    expect(await screen.findAllByText("GCM2026")).not.toHaveLength(0);
    expect(await screen.findAllByText(/Dispatch/)).not.toHaveLength(0);
    expect(await screen.findByText("Reviews ok")).toBeInTheDocument();
    expect(await screen.findAllByText("Paper")).not.toHaveLength(0);
    expect(await screen.findAllByText(/paper-primary/)).not.toHaveLength(0);
    expect(await screen.findByLabelText("Runtime posture")).toBeInTheDocument();
    expect(await screen.findAllByText("Posture")).not.toHaveLength(0);
    expect(await screen.findAllByText("Gold Breakout v1.0.0")).not.toHaveLength(0);
    expect(await screen.findByRole("tab", { name: "Checks" })).toBeInTheDocument();
    expect(await screen.findByText("Databento")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "1m" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Auto" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Fit" })).toBeInTheDocument();
    expect(await screen.findAllByText("Posture")).not.toHaveLength(0);
    expect(await screen.findAllByText("Orders")).not.toHaveLength(0);
    expect(await screen.findByRole("button", { name: "Older" })).toBeInTheDocument();
    expect(
      await screen.findAllByText(/limit 2,412\.25 \| stop 2,408\.75/),
    ).not.toHaveLength(0);
    expect(await screen.findByText("Real-time P&L chart")).toBeInTheDocument();
    expect(await screen.findByText("Per-trade P&L")).toBeInTheDocument();
    expect(await screen.findByText("Open working orders")).toBeInTheDocument();
    expect(await screen.findAllByText("Recent fills")).not.toHaveLength(0);
    expect(await screen.findByText("Trade ledger")).toBeInTheDocument();
    expect(await screen.findByText("Floating now")).toBeInTheDocument();
    expect(await screen.findByText(/Order 8102 \| limit \| filled 0/)).toBeInTheDocument();
    expect(await screen.findAllByText(/Fill fill-1 \| order 8102/)).toHaveLength(1);
    expect(await screen.findAllByText(/Trade trade-1/)).toHaveLength(2);
    expect(
      await screen.findByText("Protections on"),
    ).toBeInTheDocument();
    expect(await screen.findByText("+$97.00")).toBeInTheDocument();
    expect(await screen.findAllByText("100.0%")).toHaveLength(2);

    fireEvent.click(await screen.findByRole("tab", { name: "Setup" }));
    expect(await screen.findByText("Strategy workspace and runtime configuration")).toBeInTheDocument();
    expect(await screen.findByText("Validation passed")).toBeInTheDocument();
    expect(await screen.findByText("Load selected strategy")).toBeInTheDocument();
    expect(await screen.findByText("Runtime settings")).toBeInTheDocument();
    expect(await screen.findByText("Config file backed")).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("tab", { name: "Health" }));
    expect(await screen.findByText("Connectivity clocks")).toBeInTheDocument();
    expect(await screen.findByText("Feed and storage detail")).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("tab", { name: "Latency" }));
    expect(await screen.findByText("Latency stage breakdown")).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("tab", { name: "Journal" }));
    expect(await screen.findByText("Journal summary")).toBeInTheDocument();
    expect(await screen.findByText("Persisted operator journal and audit trail")).toBeInTheDocument();
    expect(await screen.findByText("execution:dispatch_succeeded")).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("tab", { name: "Events" }));
    expect(await screen.findByText("Recent event mix")).toBeInTheDocument();
  });

  it("surfaces reconnect and shutdown review warnings when they are active", async () => {
    installWebSocketMock();
    installFetchMock({ reconnectRequired: true, shutdownBlocked: true });

    render(<App />);

    expect(await screen.findByText("Reconnect review active")).toBeInTheDocument();
    expect(await screen.findByText("Shutdown review active")).toBeInTheDocument();
    expect(
      await screen.findAllByText(
        "existing broker-side position or working orders detected after reconnect",
      ),
    ).not.toHaveLength(0);
    expect(
      await screen.findAllByText("shutdown blocked until open position is resolved"),
    ).not.toHaveLength(0);
  });

  it("surfaces degraded feed and reconnect warnings inside the live chart module", async () => {
    installWebSocketMock();
    installFetchMock({
      reconnectRequired: true,
      shutdownBlocked: true,
      marketDataHealth: "degraded",
    });

    render(<App />);

    expect(
      await screen.findAllByText("Reconnect review"),
    ).not.toHaveLength(0);
    expect(await screen.findAllByText("Feed degraded")).not.toHaveLength(0);
    expect(
      await screen.findAllByText(
        "Databento heartbeat stale; new entries paused until feed recovery.",
      ),
    ).not.toHaveLength(0);
    expect(
      await screen.findAllByText("Shutdown review"),
    ).not.toHaveLength(0);
  });

  it("switches chart timeframes and loads older history through the chart control plane", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "5m" }));

    await waitFor(() => {
      const snapshotCalls = fetchSpy.mock.calls.filter((call) => {
        const target = call[0];
        const endpoint =
          typeof target === "string"
            ? target
            : target instanceof URL
              ? target.pathname
              : target.url;
        return String(endpoint).includes("/chart/snapshot?timeframe=5m");
      });

      expect(snapshotCalls.length).toBeGreaterThan(0);
    });

    fireEvent.click(await screen.findByRole("button", { name: "Older" }));

    await waitFor(() => {
      const historyCalls = fetchSpy.mock.calls.filter((call) => {
        const target = call[0];
        const endpoint =
          typeof target === "string"
            ? target
            : target instanceof URL
              ? target.pathname
              : target.url;
        return String(endpoint).includes("/chart/history?timeframe=5m");
      });

      expect(historyCalls.length).toBeGreaterThan(0);
    });
  });

  it("toggles live follow in the contract chart toolbar", async () => {
    installWebSocketMock();
    installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Auto" }));

    expect(await screen.findByRole("button", { name: "Manual" })).toBeInTheDocument();
  });

  it("surfaces the sample-candle fallback when live market data is not configured", async () => {
    installWebSocketMock();
    installFetchMock({ sampleDataActive: true });

    render(<App />);

    expect(await screen.findAllByText("Sample candles")).not.toHaveLength(0);
    expect(
      await screen.findByText(
        /Showing sample candles until Databento is configured\./,
      ),
    ).toBeInTheDocument();
    expect(
      await screen.findAllByText("Sample candles"),
    ).not.toHaveLength(0);
  });

  it("updates the contract chart summary when the dedicated chart stream publishes a new snapshot", async () => {
    const websocket = installWebSocketMock();
    installFetchMock();

    render(<App />);

    await screen.findAllByText("Candle");
    await waitFor(() => {
      expect(websocket.latest("/chart/stream")).not.toBeNull();
    });

    websocket.latest("/chart/stream")?.emitJson({
      kind: "snapshot",
      occurred_at: "2026-04-12T20:12:30Z",
      snapshot: {
        config: {
          available: true,
          detail: "charting GCM2026 from the loaded strategy contract",
          sample_data_active: false,
          instrument: {
            strategy_id: "gold-breakout",
            strategy_name: "Gold Breakout",
            market_family: "metals",
            market_display_name: "Gold",
            tradovate_symbol: "GCM2026",
            canonical_symbol: "GCM6",
            databento_symbols: ["GCM6"],
            summary: "Gold front month resolved to GCM2026 / GCM6",
          },
          supported_timeframes: ["1s", "1m", "5m"],
          default_timeframe: "1m",
          market_data_connection_state: "subscribed",
          market_data_health: "healthy",
          replay_caught_up: true,
          trade_ready: true,
        },
        timeframe: "1m",
        requested_limit: 240,
        bars: [
          {
            timeframe: "1m",
            open: "2412.40",
            high: "2414.95",
            low: "2412.10",
            close: "2414.80",
            volume: 276,
            closed_at: "2026-04-12T20:12:00Z",
            is_complete: false,
          },
        ],
        latest_price: "2414.80",
        latest_closed_at: "2026-04-12T20:12:00Z",
        active_position: {
          account_id: "paper-primary-id",
          symbol: "GCM2026",
          quantity: 1,
          average_price: "2410.50",
          realized_pnl: "0.00",
          unrealized_pnl: "72.40",
          protective_orders_present: true,
          captured_at: "2026-04-12T20:12:30Z",
        },
        working_orders: [],
        recent_fills: [],
        can_load_older_history: false,
      },
    });

    await waitFor(() => {
      expect(
        screen.getAllByText(
          /O 2,412\.40 \| H 2,414\.95 \| L 2,412\.10 \| C 2,414\.80/,
        ).length,
      ).toBeGreaterThan(0);
    });
    expect((await screen.findAllByText(/Build /i)).length).toBeGreaterThan(0);
  });

  it("renders recent websocket operator events from the local runtime host", async () => {
    const websocket = installWebSocketMock();
    installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("tab", { name: "Events" }));
    await screen.findByText("Local operator feed from /events");
    websocket.latest()?.emitJson({
      kind: "journal_record",
      record: {
        event_id: "evt-1",
        category: "runtime",
        action: "shutdown_blocked",
        source: "system",
        severity: "warning",
        occurred_at: "2026-04-12T20:12:03Z",
        payload: {
          reason: "shutdown blocked pending explicit review",
        },
      },
    });

    expect(await screen.findAllByText("runtime:shutdown_blocked")).toHaveLength(1);
    expect(
      await screen.findAllByText('{"reason":"shutdown blocked pending explicit review"}'),
    ).toHaveLength(1);
  });

  it("submits reconnect and shutdown review actions through runtime lifecycle commands", async () => {
    const websocket = installWebSocketMock();
    const { fetchSpy } = installFetchMock({ reconnectRequired: true, shutdownBlocked: true });
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);

    await screen.findByText("Reconnect review actions");
    websocket.latest()?.emitJson({
      kind: "command_result",
      source: "dashboard",
      result: {
        status: "executed",
        risk_status: "accepted",
        dispatch_performed: false,
        reason: "dashboard command executed",
        warnings: [],
      },
      occurred_at: "2026-04-12T20:12:03Z",
    });

    fireEvent.click(await screen.findByRole("button", { name: "Reattach bot management" }));
    expect(
      await screen.findByText("reconnect review resolved with reattach_bot_management"),
    ).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("button", { name: "Flatten first" }));
    expect(
      await screen.findByText("shutdown will continue after the broker position is flat"),
    ).toBeInTheDocument();

    const runtimeCommandCalls = fetchSpy.mock.calls.filter((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });

    expect(JSON.parse(String(runtimeCommandCalls[0]?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "resolve_reconnect_review",
        decision: "reattach_bot_management",
        contract_id: null,
        reason: "resolve reconnect review",
      },
    });
    expect(JSON.parse(String(runtimeCommandCalls[1]?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "shutdown",
        decision: "flatten_first",
        contract_id: null,
        reason: "resolve shutdown review",
      },
    });
  });

  it("loads the selected strategy through the runtime lifecycle endpoint", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("tab", { name: "Setup" }));

    fireEvent.click(await screen.findByRole("button", { name: "Load selected strategy" }));

    expect(
      await screen.findByText("loaded strategy `gold-breakout` from `strategies/sample.md`"),
    ).toBeInTheDocument();

    const runtimeCommandCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });

    expect(JSON.parse(String(runtimeCommandCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "load_strategy",
        path: "strategies/sample.md",
      },
    });
  });

  it("uploads a local strategy file through the host, refreshes the library, and selects it", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("tab", { name: "Setup" }));

    const uploadInput = (await screen.findByLabelText("Upload strategy file")) as HTMLInputElement;
    const uploadFile = new File(
      [
        "# Uploaded Breakout\n\n## Metadata\n```yaml\nschema_version: 1\nstrategy_id: uploaded-breakout\nname: Uploaded Breakout\nversion: 1.0.0\nauthor: tests\ndescription: uploaded strategy\n```\n",
      ],
      "uploaded-breakout.md",
      { type: "text/markdown" },
    );

    fireEvent.change(uploadInput, {
      target: {
        files: [uploadFile],
      },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Upload to library" }));

    expect(
      await screen.findByText(
        "Saved uploaded strategy to strategies/uploads/uploaded-breakout.md and validated it through the runtime host.",
      ),
    ).toBeInTheDocument();
    expect(
      await screen.findByDisplayValue("Uploaded Breakout"),
    ).toBeInTheDocument();
    expect(
      await screen.findByText("strategies/uploads/uploaded-breakout.md"),
    ).toBeInTheDocument();

    const uploadCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/strategies/upload");
    });

    expect(JSON.parse(String(uploadCall?.[1]?.body))).toEqual({
      source: "dashboard",
      filename: "uploaded-breakout.md",
      markdown:
        "# Uploaded Breakout\n\n## Metadata\n```yaml\nschema_version: 1\nstrategy_id: uploaded-breakout\nname: Uploaded Breakout\nversion: 1.0.0\nauthor: tests\ndescription: uploaded strategy\n```\n",
    });
  });

  it("saves runtime settings through the host-backed settings endpoint", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("tab", { name: "Setup" }));

    fireEvent.change(await screen.findByLabelText("Default strategy path"), {
      target: { value: "strategies/uploads/next-run.md" },
    });
    fireEvent.change(await screen.findByLabelText("Persistence fallback policy"), {
      target: { value: "block" },
    });
    fireEvent.change(await screen.findByLabelText("Paper account name"), {
      target: { value: "paper-secondary" },
    });
    fireEvent.change(await screen.findByLabelText("Live account name"), {
      target: { value: "live-ops" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Save runtime settings" }));

    expect(
      await screen.findByText("saved runtime settings for the next restart"),
    ).toBeInTheDocument();

    const settingsCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/settings") && call[1]?.method === "POST";
    });

    expect(JSON.parse(String(settingsCall?.[1]?.body))).toEqual({
      source: "dashboard",
      settings: {
        startup_mode: "observation",
        default_strategy_path: "strategies/uploads/next-run.md",
        allow_sqlite_fallback: false,
        paper_account_name: "paper-secondary",
        live_account_name: "live-ops",
      },
    });
  });

  it("sends pause through the runtime lifecycle endpoint and updates the control surface", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Pause" }));

    await screen.findByText("runtime paused");
    expect(await screen.findByRole("button", { name: "Resume" })).toBeInTheDocument();

    const runtimeCommandCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });

    expect(runtimeCommandCall).toBeDefined();
    expect(JSON.parse(String(runtimeCommandCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "pause",
      },
    });
  });

  it("posts the operator new-entry gate through the runtime lifecycle endpoint", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("New entry gate reason"), {
      target: { value: "let the current runner finish without adding size" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Block entries" }));

    expect(
      await screen.findByText(
        "new entries disabled: let the current runner finish without adding size",
      ),
    ).toBeInTheDocument();
    expect(await screen.findAllByText("Entries off")).not.toHaveLength(0);
    expect(await screen.findByRole("button", { name: "Allow entries" })).toBeEnabled();

    const runtimeCommandCalls = fetchSpy.mock.calls.filter((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });
    const disableCall = runtimeCommandCalls[runtimeCommandCalls.length - 1];
    expect(JSON.parse(String(disableCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "set_new_entries_enabled",
        enabled: false,
        reason: "let the current runner finish without adding size",
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: "Allow entries" }));

    expect(await screen.findByText("new entries enabled")).toBeInTheDocument();
    expect(await screen.findAllByText("Entries on")).not.toHaveLength(0);

    const updatedRuntimeCommandCalls = fetchSpy.mock.calls.filter((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });
    const enableCall = updatedRuntimeCommandCalls[updatedRuntimeCommandCalls.length - 1];
    expect(JSON.parse(String(enableCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "set_new_entries_enabled",
        enabled: true,
        reason: "dashboard operator entry gate",
      },
    });
  });

  it("requires confirmation before posting close position", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("Flatten position reason"), {
      target: { value: "dashboard safety close" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Flatten" }));

    expect(confirmSpy).toHaveBeenCalledWith(
      "Flatten the active broker position now? The runtime host will resolve the current contract from the synchronized broker snapshot and dispatch the audited flatten path.",
    );

    await waitFor(() => {
      const runtimeCommandCalls = fetchSpy.mock.calls.filter((call) => {
        const target = call[0];
        const endpoint =
          typeof target === "string"
            ? target
            : target instanceof URL
              ? target.pathname
              : target.url;
        return endpoint.endsWith("/runtime/commands");
      });

      expect(runtimeCommandCalls).toHaveLength(0);
    });
  });

  it("posts close position with the dashboard source once confirmation is accepted", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("Flatten position reason"), {
      target: { value: "dashboard safety close" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Flatten" }));

    expect(await screen.findByText("close position command dispatched")).toBeInTheDocument();

    const runtimeCommandCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });

    expect(JSON.parse(String(runtimeCommandCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "close_position",
        contract_id: null,
        reason: "dashboard safety close",
      },
    });
  });

  it("posts cancel working orders once confirmation is accepted", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("Cancel working orders reason"), {
      target: { value: "dashboard cancel stale orders" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Cancel all" }));

    expect(await screen.findByText("working-order cancellation dispatched")).toBeInTheDocument();

    const runtimeCommandCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });

    expect(JSON.parse(String(runtimeCommandCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "cancel_working_orders",
        reason: "dashboard cancel stale orders",
      },
    });
  });

  it("posts manual entry through the runtime lifecycle endpoint once confirmation is accepted", async () => {
    installWebSocketMock();
    const { fetchSpy } = installFetchMock();
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("Manual entry side"), {
      target: { value: "sell" },
    });
    fireEvent.change(await screen.findByLabelText("Manual entry quantity"), {
      target: { value: "2" },
    });
    fireEvent.change(await screen.findByLabelText("Manual entry tick size"), {
      target: { value: "0.1" },
    });
    fireEvent.change(await screen.findByLabelText("Manual entry reference price"), {
      target: { value: "2412.25" },
    });
    fireEvent.change(await screen.findByLabelText("Manual entry tick value"), {
      target: { value: "10" },
    });
    fireEvent.change(await screen.findByLabelText("Manual entry reason"), {
      target: { value: "dashboard breakout probe" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Send order" }));

    expect(await screen.findByText("manual entry command dispatched")).toBeInTheDocument();

    const runtimeCommandCall = fetchSpy.mock.calls.find((call) => {
      const target = call[0];
      const endpoint =
        typeof target === "string"
          ? target
          : target instanceof URL
            ? target.pathname
            : target.url;
      return endpoint.endsWith("/runtime/commands");
    });

    expect(JSON.parse(String(runtimeCommandCall?.[1]?.body))).toEqual({
      source: "dashboard",
      command: {
        kind: "manual_entry",
        side: "sell",
        quantity: 2,
        tick_size: "0.1",
        entry_reference_price: "2412.25",
        tick_value_usd: "10",
        reason: "dashboard breakout probe",
      },
    });
  });
});
