import {
  startTransition,
  useEffect,
  useEffectEvent,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import {
  controlApiEventsUrl,
  loadDashboardSnapshot,
  parseControlApiEvent,
  sendLifecycleCommand,
  type DashboardSnapshot,
  type LifecycleCommandResult,
} from "../lib/api";
import {
  MAX_RECENT_EVENTS,
  feedbackToneFromHttpStatus,
  mergeEventIntoSnapshot,
  mergeLifecycleResponseIntoSnapshot,
  toEventFeedItem,
} from "../lib/dashboardProjection";
import type {
  CommandFeedback,
  CommandOptions,
  EventFeedViewModel,
  ViewModel,
} from "../dashboardModels";
import type { RuntimeLifecycleCommand } from "../types/controlApi";

const REFRESH_INTERVAL_MS = 5_000;
const EVENTS_RECONNECT_DELAY_MS = 1_500;

const INITIAL_VIEW_MODEL: ViewModel = {
  snapshot: null,
  loadState: "idle",
  error: null,
  lastAttemptedAt: null,
};

const INITIAL_EVENT_FEED_VIEW_MODEL: EventFeedViewModel = {
  connectionState: "connecting",
  recentEvents: [],
  lastEventAt: null,
  error: null,
};

export interface DashboardRuntimeHostController {
  viewModel: ViewModel;
  eventFeed: EventFeedViewModel;
  commandFeedback: CommandFeedback | null;
  setCommandFeedback: Dispatch<SetStateAction<CommandFeedback | null>>;
  pendingAction: string | null;
  setPendingAction: Dispatch<SetStateAction<string | null>>;
  refreshSnapshot: (signal?: AbortSignal) => Promise<void>;
  executeLifecycleCommand: (
    command: RuntimeLifecycleCommand,
    options: CommandOptions,
  ) => Promise<LifecycleCommandResult | null>;
  updateSnapshot: (
    updater: (snapshot: DashboardSnapshot | null) => DashboardSnapshot | null,
  ) => void;
}

export function useDashboardRuntimeHost(): DashboardRuntimeHostController {
  const [viewModel, setViewModel] = useState<ViewModel>(INITIAL_VIEW_MODEL);
  const [eventFeed, setEventFeed] = useState<EventFeedViewModel>(INITIAL_EVENT_FEED_VIEW_MODEL);
  const [commandFeedback, setCommandFeedback] = useState<CommandFeedback | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);

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

  const updateSnapshot = useEffectEvent(
    (updater: (snapshot: DashboardSnapshot | null) => DashboardSnapshot | null) => {
      setViewModel((current) => ({
        ...current,
        snapshot: updater(current.snapshot),
        loadState: "ready",
        error: null,
        lastAttemptedAt: new Date().toISOString(),
      }));
    },
  );

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

        updateSnapshot((snapshot) =>
          refreshedSnapshot ?? mergeLifecycleResponseIntoSnapshot(snapshot, result.response),
        );
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

  useEffect(() => {
    const controller = new AbortController();
    void refreshSnapshot(controller.signal);

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
          updateSnapshot((snapshot) => mergeEventIntoSnapshot(snapshot, event));
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

  return {
    viewModel,
    eventFeed,
    commandFeedback,
    setCommandFeedback,
    pendingAction,
    setPendingAction,
    refreshSnapshot,
    executeLifecycleCommand,
    updateSnapshot,
  };
}
