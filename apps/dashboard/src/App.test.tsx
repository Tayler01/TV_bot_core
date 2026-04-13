import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json",
    },
  });
}

function installFetchMock(snapshotOverrides?: {
  reconnectRequired?: boolean;
  shutdownBlocked?: boolean;
}) {
  const reconnectRequired = snapshotOverrides?.reconnectRequired ?? false;
  const shutdownBlocked = snapshotOverrides?.shutdownBlocked ?? false;

  const status = {
    mode: "paper",
    arm_state: "armed",
    warmup_status: "ready",
    strategy_loaded: true,
    hard_override_active: false,
    current_strategy: {
      path: "strategies/sample.md",
      title: "Sample",
      strategy_id: "gold-breakout",
      name: "Gold Breakout",
      version: "1.0.0",
      market_family: "metals",
      warning_count: 0,
    },
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
          connection_state: "connected",
          health: "healthy",
          feed_statuses: [
            {
              instrument_symbol: "GCM2026",
              feed: "ohlcv_1m",
              state: "ready",
              last_event_at: "2026-04-12T20:11:00Z",
              detail: "feed ready",
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
          last_disconnect_reason: null,
          updated_at: "2026-04-12T20:11:00Z",
        },
      },
      warmup_requested: true,
      warmup_mode: "live_only",
      replay_caught_up: true,
      trade_ready: true,
      updated_at: "2026-04-12T20:11:00Z",
    },
    market_data_detail: null,
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
      feed_degraded: false,
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

  const history = {
    projection: {
      total_strategy_run_records: 1,
      total_order_records: 4,
      total_fill_records: 2,
      total_position_records: 2,
      total_pnl_snapshot_records: 2,
      total_trade_summary_records: 1,
      working_order_ids: ["entry-1"],
      open_position_symbols: ["GCM2026"],
      open_trade_ids: ["trade-1"],
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

  const health = {
    status: "healthy",
    system_health: status.system_health,
    latest_trade_latency: status.latest_trade_latency,
  };

  const strategyLibrary = {
    scanned_roots: ["strategies/examples"],
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

  function validationForPath(path: string) {
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

    if (endpoint.endsWith("/status")) {
      return jsonResponse(status);
    }

    if (endpoint.endsWith("/readiness")) {
      return jsonResponse(readiness);
    }

    if (endpoint.endsWith("/history")) {
      return jsonResponse(history);
    }

    if (endpoint.endsWith("/health")) {
      return jsonResponse(health);
    }

    if (endpoint.endsWith("/strategies")) {
      return jsonResponse(strategyLibrary);
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
});

describe("App", () => {
  it("renders the paper overview from the local control API snapshot", async () => {
    installFetchMock();

    render(<App />);

    expect(
      await screen.findByText("Operator dashboard for the local runtime host"),
    ).toBeInTheDocument();
    expect(await screen.findAllByText("Paper")).toHaveLength(3);
    expect(await screen.findByText("paper-primary")).toBeInTheDocument();
    expect(await screen.findAllByText("Gold Breakout v1.0.0")).toHaveLength(2);
    expect(await screen.findByText("Load selected strategy")).toBeInTheDocument();
    expect(await screen.findByText("Validation passed")).toBeInTheDocument();
    expect(await screen.findByText("Grouped pre-arm checks")).toBeInTheDocument();
    expect(
      await screen.findByText(
        "Protective brackets available and operator overrides are clear.",
      ),
    ).toBeInTheDocument();
    expect(await screen.findByText("+$97.00")).toBeInTheDocument();
  });

  it("surfaces reconnect and shutdown review warnings when they are active", async () => {
    installFetchMock({ reconnectRequired: true, shutdownBlocked: true });

    render(<App />);

    expect(await screen.findByText("Reconnect review active")).toBeInTheDocument();
    expect(await screen.findByText("Shutdown review active")).toBeInTheDocument();
    expect(
      await screen.findByText(
        "existing broker-side position or working orders detected after reconnect",
      ),
    ).toBeInTheDocument();
    expect(
      await screen.findByText("shutdown blocked until open position is resolved"),
    ).toBeInTheDocument();
  });

  it("loads the selected strategy through the runtime lifecycle endpoint", async () => {
    const { fetchSpy } = installFetchMock();

    render(<App />);

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

  it("sends pause through the runtime lifecycle endpoint and updates the control surface", async () => {
    const { fetchSpy } = installFetchMock();

    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "Pause runtime" }));

    await screen.findByText("runtime paused");
    expect(await screen.findByRole("button", { name: "Resume runtime" })).toBeInTheDocument();

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

  it("requires confirmation before posting a flatten command", async () => {
    const { fetchSpy } = installFetchMock();
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("Active contract id"), {
      target: { value: "4444" },
    });
    fireEvent.change(await screen.findByLabelText("Flatten reason"), {
      target: { value: "dashboard safety flatten" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Flatten position" }));

    expect(confirmSpy).toHaveBeenCalledWith(
      "Flatten contract 4444 now? Existing broker-managed exposure will be liquidated.",
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

  it("posts flatten with the dashboard source once confirmation is accepted", async () => {
    const { fetchSpy } = installFetchMock();
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);

    fireEvent.change(await screen.findByLabelText("Active contract id"), {
      target: { value: "4444" },
    });
    fireEvent.change(await screen.findByLabelText("Flatten reason"), {
      target: { value: "dashboard safety flatten" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "Flatten position" }));

    expect(await screen.findByText("flatten command dispatched")).toBeInTheDocument();

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
        kind: "flatten",
        contract_id: 4444,
        reason: "dashboard safety flatten",
      },
    });
  });
});
