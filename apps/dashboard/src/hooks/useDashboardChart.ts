import {
  startTransition,
  useEffect,
  useEffectEvent,
  useMemo,
  useState,
} from "react";

import type { DashboardSnapshot } from "../lib/api";
import {
  controlApiChartStreamUrl,
  loadChartConfig,
  loadChartHistory,
  loadChartSnapshot,
  parseRuntimeChartStreamEvent,
} from "../lib/api";
import { mergeChartBars } from "../lib/chartAdapter";
import type { ChartViewModel } from "../dashboardModels";
import type {
  RuntimeChartSnapshot,
  Timeframe,
} from "../types/controlApi";

const DEFAULT_VIEWPORT_WIDTH = 1440;
const CHART_RECONNECT_DELAY_MS = 1_500;

const INITIAL_CHART_VIEW_MODEL: ChartViewModel = {
  config: null,
  snapshot: null,
  selectedTimeframe: null,
  loadState: "idle",
  historyState: "idle",
  streamState: "closed",
  error: null,
  historyError: null,
  lastStreamedAt: null,
};

export interface DashboardChartController {
  chartViewModel: ChartViewModel;
  setSelectedTimeframe: (timeframe: Timeframe) => void;
  refreshChart: (signal?: AbortSignal) => Promise<void>;
  loadOlderHistory: () => Promise<void>;
}

function snapshotScopeKey(snapshot: DashboardSnapshot | null): string {
  if (!snapshot) {
    return "runtime-unavailable";
  }

  return [
    snapshot.status.current_strategy?.strategy_id ?? "no-strategy",
    snapshot.status.instrument_mapping?.tradovate_symbol ?? "no-symbol",
    snapshot.status.mode,
  ].join(":");
}

function resolvedInitialTimeframe(
  current: Timeframe | null,
  supported: Timeframe[],
  fallback: Timeframe | null,
): Timeframe | null {
  if (current && supported.includes(current)) {
    return current;
  }

  if (fallback && supported.includes(fallback)) {
    return fallback;
  }

  return supported[0] ?? fallback ?? null;
}

function mergeIncomingSnapshot(
  current: RuntimeChartSnapshot | null,
  incoming: RuntimeChartSnapshot,
): RuntimeChartSnapshot {
  const mergedBars = current
    ? mergeChartBars(current.bars, incoming.bars)
    : incoming.bars;
  const hasExtendedHistory = current ? current.bars.length > incoming.bars.length : false;

  return {
    ...incoming,
    bars: mergedBars,
    can_load_older_history: hasExtendedHistory
      ? current?.can_load_older_history ?? incoming.can_load_older_history
      : incoming.can_load_older_history,
  };
}

function chartViewportWidth(): number {
  if (typeof window === "undefined") {
    return DEFAULT_VIEWPORT_WIDTH;
  }

  return Math.max(window.innerWidth, 390);
}

function chartVisibleBarsTarget(timeframe: Timeframe, viewportWidth = chartViewportWidth()): number {
  if (timeframe === "1s") {
    if (viewportWidth >= 1280) {
      return 150;
    }

    if (viewportWidth >= 900) {
      return 118;
    }

    return 72;
  }

  if (timeframe === "5m") {
    if (viewportWidth >= 1280) {
      return 112;
    }

    if (viewportWidth >= 900) {
      return 92;
    }

    return 56;
  }

  if (viewportWidth >= 1280) {
    return 132;
  }

  if (viewportWidth >= 900) {
    return 104;
  }

  return 64;
}

function chartBarsForTwoHours(timeframe: Timeframe): number {
  switch (timeframe) {
    case "1s":
      return 7_200;
    case "5m":
      return 24;
    case "1m":
    default:
      return 120;
  }
}

function chartSnapshotLimit(timeframe: Timeframe, viewportWidth = chartViewportWidth()): number {
  return Math.max(chartVisibleBarsTarget(timeframe, viewportWidth), chartBarsForTwoHours(timeframe));
}

function chartHistoryPageSize(timeframe: Timeframe, viewportWidth = chartViewportWidth()): number {
  return Math.max(
    Math.round(chartVisibleBarsTarget(timeframe, viewportWidth) * 1.25),
    Math.round(chartBarsForTwoHours(timeframe) / 2),
  );
}

export function useDashboardChart(
  snapshot: DashboardSnapshot | null,
): DashboardChartController {
  const [chartViewModel, setChartViewModel] = useState<ChartViewModel>(INITIAL_CHART_VIEW_MODEL);

  const scopeKey = useMemo(() => snapshotScopeKey(snapshot), [snapshot]);

  const refreshChart = useEffectEvent(async (signal?: AbortSignal) => {
    setChartViewModel((current) => ({
      ...current,
      loadState: current.snapshot ? "ready" : "loading",
      error: null,
    }));

    try {
      const config = await loadChartConfig(signal);
      const nextTimeframe = resolvedInitialTimeframe(
        chartViewModel.selectedTimeframe,
        config.supported_timeframes,
        config.default_timeframe,
      );

      if (!config.available || nextTimeframe === null) {
        startTransition(() => {
          setChartViewModel((current) => ({
            ...current,
            config,
            snapshot: null,
            selectedTimeframe: nextTimeframe,
            loadState: "ready",
            error: null,
            historyError: null,
            historyState: "idle",
          }));
        });
        return;
      }

      const currentSnapshot = await loadChartSnapshot(
        nextTimeframe,
        chartSnapshotLimit(nextTimeframe),
        signal,
      );

      startTransition(() => {
        setChartViewModel((current) => ({
          ...current,
          config,
          snapshot: currentSnapshot,
          selectedTimeframe: nextTimeframe,
          loadState: "ready",
          error: null,
          historyError: null,
          historyState: "idle",
        }));
      });
    } catch (error) {
      if (signal?.aborted) {
        return;
      }

      setChartViewModel((current) => ({
        ...current,
        loadState: current.snapshot ? "ready" : "error",
        error:
          error instanceof Error
            ? error.message
            : "Dashboard chart failed to read the local control plane.",
      }));
    }
  });

  const setSelectedTimeframe = useEffectEvent((timeframe: Timeframe) => {
    setChartViewModel((current) => ({
      ...current,
      selectedTimeframe: timeframe,
      historyError: null,
      historyState: "idle",
    }));
  });

  const loadOlderHistory = useEffectEvent(async () => {
    const activeSnapshot = chartViewModel.snapshot;
    const timeframe = chartViewModel.selectedTimeframe;
    const earliestBar = activeSnapshot?.bars[0];

    if (
      !activeSnapshot ||
      !timeframe ||
      !earliestBar ||
      !activeSnapshot.can_load_older_history ||
      chartViewModel.historyState === "loading"
    ) {
      return;
    }

    setChartViewModel((current) => ({
      ...current,
      historyState: "loading",
      historyError: null,
    }));

    try {
      const history = await loadChartHistory(
        timeframe,
        earliestBar.closed_at,
        chartHistoryPageSize(timeframe),
      );

      setChartViewModel((current) => {
        if (!current.snapshot) {
          return {
            ...current,
            historyState: "ready",
            historyError: null,
          };
        }

        return {
          ...current,
          config: history.config,
          snapshot: {
            ...current.snapshot,
            config: history.config,
            bars: mergeChartBars(history.bars, current.snapshot.bars),
            can_load_older_history: history.can_load_older_history,
          },
          historyState: "ready",
          historyError: null,
        };
      });
    } catch (error) {
      setChartViewModel((current) => ({
        ...current,
        historyState: "error",
        historyError:
          error instanceof Error
            ? error.message
            : "Dashboard chart could not load older buffered history.",
      }));
    }
  });

  useEffect(() => {
    const controller = new AbortController();
    void refreshChart(controller.signal);

    return () => {
      controller.abort();
    };
  }, [scopeKey]);

  useEffect(() => {
    const controller = new AbortController();
    const timeframe = chartViewModel.selectedTimeframe;

    if (!chartViewModel.config?.available || !timeframe) {
      return () => {
        controller.abort();
      };
    }

    void (async () => {
      setChartViewModel((current) => ({
        ...current,
        loadState: current.snapshot ? "ready" : "loading",
        error: null,
      }));

      try {
        const currentSnapshot = await loadChartSnapshot(
          timeframe,
          chartSnapshotLimit(timeframe),
          controller.signal,
        );

        startTransition(() => {
          setChartViewModel((current) => ({
            ...current,
            config: currentSnapshot.config,
            snapshot: currentSnapshot,
            loadState: "ready",
            error: null,
            historyError: null,
            historyState: "idle",
          }));
        });
      } catch (error) {
        if (controller.signal.aborted) {
          return;
        }

        setChartViewModel((current) => ({
          ...current,
          loadState: current.snapshot ? "ready" : "error",
          error:
            error instanceof Error
              ? error.message
              : "Dashboard chart failed to load the selected timeframe.",
        }));
      }
    })();

    return () => {
      controller.abort();
    };
  }, [chartViewModel.selectedTimeframe, scopeKey]);

  useEffect(() => {
    if (typeof WebSocket === "undefined") {
      setChartViewModel((current) => ({
        ...current,
        streamState: "unsupported",
      }));
      return;
    }

    if (!chartViewModel.config?.available || !chartViewModel.selectedTimeframe) {
      setChartViewModel((current) => ({
        ...current,
        streamState: "closed",
      }));
      return;
    }

    let active = true;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let initialConnectTimer: number | null = window.setTimeout(() => {
      initialConnectTimer = null;
      connect();
    }, 0);

    const connect = () => {
      if (!active || !chartViewModel.selectedTimeframe) {
        return;
      }

      setChartViewModel((current) => ({
        ...current,
        streamState: "connecting",
      }));

      socket = new WebSocket(
        controlApiChartStreamUrl(
          chartViewModel.selectedTimeframe,
          chartSnapshotLimit(chartViewModel.selectedTimeframe),
        ),
      );

      socket.onopen = () => {
        if (!active) {
          return;
        }

        setChartViewModel((current) => ({
          ...current,
          streamState: "open",
        }));
      };

      socket.onmessage = (message) => {
        if (!active || typeof message.data !== "string") {
          return;
        }

        try {
          const event = parseRuntimeChartStreamEvent(message.data);

          if (event.kind !== "snapshot") {
            return;
          }

          setChartViewModel((current) => ({
            ...current,
            config: event.snapshot.config,
            snapshot: mergeIncomingSnapshot(current.snapshot, event.snapshot),
            streamState: "open",
            lastStreamedAt: event.occurred_at,
            error: null,
          }));
        } catch (error) {
          setChartViewModel((current) => ({
            ...current,
            streamState: "error",
            error:
              error instanceof Error
                ? error.message
                : "Dashboard chart could not parse a chart stream event.",
          }));
        }
      };

      socket.onerror = () => {
        if (!active) {
          return;
        }

        setChartViewModel((current) => ({
          ...current,
          streamState: "error",
          error: current.error ?? "Chart stream reported a transport error.",
        }));
      };

      socket.onclose = () => {
        if (!active) {
          return;
        }

        setChartViewModel((current) => ({
          ...current,
          streamState: "closed",
        }));
        reconnectTimer = window.setTimeout(() => {
          reconnectTimer = null;
          connect();
        }, CHART_RECONNECT_DELAY_MS);
      };
    };

    return () => {
      active = false;
      if (initialConnectTimer !== null) {
        window.clearTimeout(initialConnectTimer);
      }
      if (reconnectTimer !== null) {
        window.clearTimeout(reconnectTimer);
      }
      socket?.close();
    };
  }, [chartViewModel.selectedTimeframe, chartViewModel.config?.available, scopeKey]);

  return {
    chartViewModel,
    setSelectedTimeframe,
    refreshChart,
    loadOlderHistory,
  };
}
