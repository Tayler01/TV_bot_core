import {
  startTransition,
  useEffect,
  useEffectEvent,
  useRef,
  useState,
} from "react";

import {
  controlApiEventsUrl,
  loadDashboardSnapshot,
  loadStrategyLibrary,
  parseControlApiEvent,
  sendLifecycleCommand,
  updateRuntimeSettings,
  uploadStrategyMarkdown,
  validateStrategyPath,
  type DashboardSnapshot,
  type LifecycleCommandResult,
} from "../lib/api";
import {
  MAX_RECENT_EVENTS,
  feedbackToneFromHttpStatus,
  mergeEventIntoSnapshot,
  mergeLifecycleResponseIntoSnapshot,
  runtimeSettingsRequestFromDraft,
  selectStrategyPath,
  settingsDraftFromSnapshot,
  toEventFeedItem,
} from "../lib/dashboardProjection";
import type {
  CommandFeedback,
  CommandOptions,
  EventFeedViewModel,
  RuntimeSettingsDraft,
  StrategySummaryViewModel,
  ViewModel,
} from "../dashboardModels";
import type {
  RuntimeLifecycleCommand,
  RuntimeMode,
} from "../types/controlApi";

const REFRESH_INTERVAL_MS = 5_000;
const EVENTS_RECONNECT_DELAY_MS = 1_500;

const INITIAL_VIEW_MODEL: ViewModel = {
  snapshot: null,
  loadState: "idle",
  error: null,
  lastAttemptedAt: null,
};

const INITIAL_STRATEGY_VIEW_MODEL: StrategySummaryViewModel = {
  library: null,
  validation: null,
  libraryError: null,
  validationError: null,
  libraryState: "idle",
  validationState: "idle",
  selectedPath: "",
};

const INITIAL_EVENT_FEED_VIEW_MODEL: EventFeedViewModel = {
  connectionState: "connecting",
  recentEvents: [],
  lastEventAt: null,
  error: null,
};

async function readStrategyUploadFile(file: File): Promise<string> {
  if (typeof file.text === "function") {
    return await file.text();
  }

  if (typeof FileReader !== "undefined") {
    return await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        resolve(typeof reader.result === "string" ? reader.result : "");
      };
      reader.onerror = () => {
        reject(reader.error ?? new Error("Dashboard failed to read the selected strategy file."));
      };
      reader.readAsText(file);
    });
  }

  return String(file);
}

export function useDashboardController() {
  const strategyUploadInputRef = useRef<HTMLInputElement | null>(null);
  const settingsDraftRef = useRef<RuntimeSettingsDraft | null>(null);
  const [viewModel, setViewModel] = useState<ViewModel>(INITIAL_VIEW_MODEL);
  const [strategyViewModel, setStrategyViewModel] = useState<StrategySummaryViewModel>(
    INITIAL_STRATEGY_VIEW_MODEL,
  );
  const [eventFeed, setEventFeed] = useState<EventFeedViewModel>(INITIAL_EVENT_FEED_VIEW_MODEL);
  const [commandFeedback, setCommandFeedback] = useState<CommandFeedback | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [newEntriesReason, setNewEntriesReason] = useState("operator gate");
  const [closePositionReason, setClosePositionReason] = useState("flatten position");
  const [manualEntrySide, setManualEntrySide] = useState<"buy" | "sell">("buy");
  const [manualEntryQuantity, setManualEntryQuantity] = useState("1");
  const [manualEntryTickSize, setManualEntryTickSize] = useState("0.1");
  const [manualEntryReferencePrice, setManualEntryReferencePrice] = useState("");
  const [manualEntryTickValueUsd, setManualEntryTickValueUsd] = useState("");
  const [manualEntryReason, setManualEntryReason] = useState("manual entry");
  const [cancelWorkingOrdersReason, setCancelWorkingOrdersReason] = useState(
    "cancel working orders",
  );
  const [reconnectReason, setReconnectReason] = useState("resolve reconnect review");
  const [shutdownReason, setShutdownReason] = useState("resolve shutdown review");
  const [selectedStrategyUploadFile, setSelectedStrategyUploadFile] = useState<File | null>(null);
  const [settingsDraft, setSettingsDraft] = useState<RuntimeSettingsDraft | null>(null);
  const [settingsDirty, setSettingsDirty] = useState(false);

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

        setViewModel((current) => ({
          ...current,
          snapshot:
            refreshedSnapshot ??
            mergeLifecycleResponseIntoSnapshot(current.snapshot, result.response),
          loadState: "ready",
          error: null,
          lastAttemptedAt: new Date().toISOString(),
        }));
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

  const executeReconnectDecision = useEffectEvent(
    async (decision: "close_position" | "leave_broker_protected" | "reattach_bot_management") => {
      const confirmMessage =
        decision === "close_position"
          ? "Close the active broker position as part of reconnect recovery?"
          : undefined;

      const result = await executeLifecycleCommand(
        {
          kind: "resolve_reconnect_review",
          decision,
          contract_id: null,
          reason: reconnectReason.trim() || null,
        },
        {
          pendingLabel: `Resolving reconnect review with ${decision}`,
          confirmMessage,
        },
      );

      if (result?.httpStatus === 200) {
        setReconnectReason("resolve reconnect review");
      }
    },
  );

  const executeShutdownDecision = useEffectEvent(
    async (decision: "flatten_first" | "leave_broker_protected") => {
      const confirmMessage =
        decision === "flatten_first"
          ? "Request flatten-first shutdown handling now? The runtime will flatten and then continue shutdown once the broker position is flat."
          : "Approve shutdown while leaving broker-protected positions in place?";

      const result = await executeLifecycleCommand(
        {
          kind: "shutdown",
          decision,
          contract_id: null,
          reason: shutdownReason.trim() || null,
        },
        {
          pendingLabel: `Submitting shutdown review decision ${decision}`,
          confirmMessage,
        },
      );

      if (result?.httpStatus === 200) {
        setShutdownReason("resolve shutdown review");
      }
    },
  );

  const updateNewEntriesEnabled = useEffectEvent(async (enabled: boolean) => {
    const result = await executeLifecycleCommand(
      {
        kind: "set_new_entries_enabled",
        enabled,
        reason: newEntriesReason.trim() || null,
      },
      {
        pendingLabel: enabled ? "Re-enabling new entries" : "Disabling new entries",
        confirmMessage: enabled
          ? undefined
          : "Disable new entries now? Existing positions can still be managed, but fresh entry requests will stay blocked until you re-enable them.",
      },
    );

    if (result?.httpStatus === 200) {
      setNewEntriesReason("dashboard operator entry gate");
    }
  });

  const refreshStrategyLibrary = useEffectEvent(async (signal?: AbortSignal) => {
    setStrategyViewModel((current) => ({
      ...current,
      libraryState: "loading",
      libraryError: null,
    }));

    try {
      const library = await loadStrategyLibrary(signal);
      setStrategyViewModel((current) => ({
        ...current,
        library,
        libraryState: "ready",
        libraryError: null,
        selectedPath: selectStrategyPath(library, current.selectedPath),
      }));
    } catch (error) {
      if (signal?.aborted) {
        return;
      }

      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to read the local strategy library.";

      setStrategyViewModel((current) => ({
        ...current,
        libraryState: current.library ? "ready" : "error",
        libraryError: message,
      }));
    }
  });

  const saveRuntimeSettings = useEffectEvent(async () => {
    if (!settingsDraft) {
      return;
    }

    setPendingAction("Saving runtime settings");
    setCommandFeedback(null);

    try {
      const result = await updateRuntimeSettings({
        source: "dashboard",
        settings: runtimeSettingsRequestFromDraft(settingsDraft),
      });
      let refreshedSnapshot: DashboardSnapshot | null = null;

      try {
        refreshedSnapshot = await loadDashboardSnapshot();
      } catch {
        refreshedSnapshot = null;
      }

      setViewModel((current) => ({
        ...current,
        snapshot:
          refreshedSnapshot ??
          (current.snapshot
            ? {
                ...current.snapshot,
                settings: result.settings,
                fetchedAt: new Date().toISOString(),
              }
            : null),
        loadState: "ready",
        error: null,
        lastAttemptedAt: new Date().toISOString(),
      }));
      const nextDraft = settingsDraftFromSnapshot(result.settings);
      settingsDraftRef.current = nextDraft;
      setSettingsDraft(nextDraft);
      setSettingsDirty(false);
      setCommandFeedback({
        tone: result.settings.persistence_mode === "config_file" ? "healthy" : "warning",
        message: result.message,
      });
    } catch (error) {
      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to save runtime settings through the local control API.";
      setCommandFeedback({
        tone: "danger",
        message,
      });
    } finally {
      setPendingAction(null);
    }
  });

  const refreshStrategyValidation = useEffectEvent(
    async (path: string, signal?: AbortSignal) => {
      if (!path) {
        setStrategyViewModel((current) => ({
          ...current,
          validation: null,
          validationError: null,
          validationState: "idle",
        }));
        return;
      }

      setStrategyViewModel((current) => ({
        ...current,
        validationState: "loading",
        validationError: null,
      }));

      try {
        const validation = await validateStrategyPath(path, signal);
        setStrategyViewModel((current) => ({
          ...current,
          validation,
          validationError: null,
          validationState: "ready",
        }));
      } catch (error) {
        if (signal?.aborted) {
          return;
        }

        const message =
          error instanceof Error
            ? error.message
            : "Dashboard failed to validate the selected strategy.";

        setStrategyViewModel((current) => ({
          ...current,
          validation: null,
          validationError: message,
          validationState: "error",
        }));
      }
    },
  );

  const uploadSelectedStrategyFile = useEffectEvent(async () => {
    if (!selectedStrategyUploadFile) {
      return;
    }

    setPendingAction("Uploading strategy into the local runtime library");
    setCommandFeedback(null);

    try {
      const markdown = await readStrategyUploadFile(selectedStrategyUploadFile);
      const validation = await uploadStrategyMarkdown(
        selectedStrategyUploadFile.name,
        markdown,
      );

      await refreshStrategyLibrary();
      setStrategyViewModel((current) => ({
        ...current,
        selectedPath: validation.path,
        validation,
        validationError: null,
        validationState: "ready",
      }));
      setSelectedStrategyUploadFile(null);
      if (strategyUploadInputRef.current) {
        strategyUploadInputRef.current.value = "";
      }

      setCommandFeedback({
        tone: validation.valid
          ? validation.warnings.length > 0
            ? "warning"
            : "healthy"
          : "warning",
        message: validation.valid
          ? `Saved uploaded strategy to ${validation.display_path} and validated it through the runtime host.`
          : `Saved uploaded strategy to ${validation.display_path}, but validation found ${validation.errors.length} error(s).`,
      });
    } catch (error) {
      const message =
        error instanceof Error
          ? error.message
          : "Dashboard failed to upload the selected strategy file.";

      setCommandFeedback({
        tone: "danger",
        message,
      });
    } finally {
      setPendingAction(null);
    }
  });

  useEffect(() => {
    const controller = new AbortController();
    void refreshSnapshot(controller.signal);
    void refreshStrategyLibrary(controller.signal);

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
          setViewModel((current) => ({
            ...current,
            snapshot: mergeEventIntoSnapshot(current.snapshot, event),
          }));
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

  useEffect(() => {
    if (!strategyViewModel.selectedPath) {
      return;
    }

    const controller = new AbortController();
    void refreshStrategyValidation(strategyViewModel.selectedPath, controller.signal);

    return () => {
      controller.abort();
    };
  }, [strategyViewModel.selectedPath]);

  const snapshot = viewModel.snapshot;
  const updateSettingsDraft = useEffectEvent(
    (updater: (draft: RuntimeSettingsDraft) => RuntimeSettingsDraft) => {
      if (!snapshot) {
        return;
      }

      setSettingsDirty(true);
      setSettingsDraft((current) => {
        const next = updater(
          current ?? settingsDraftRef.current ?? settingsDraftFromSnapshot(snapshot.settings),
        );
        settingsDraftRef.current = next;
        return next;
      });
    },
  );

  useEffect(() => {
    if (!snapshot || settingsDirty) {
      return;
    }

    const nextDraft = settingsDraftFromSnapshot(snapshot.settings);
    settingsDraftRef.current = nextDraft;
    setSettingsDraft(nextDraft);
  }, [settingsDirty, snapshot]);

  const handleSetMode = (mode: RuntimeMode) => {
    const options =
      mode === "live"
        ? {
            pendingLabel: "Switching runtime to live mode",
            confirmMessage:
              "Switch the runtime into LIVE mode? Paper and live are intentionally separated. Continue?",
          }
        : {
            pendingLabel:
              mode === "paper"
                ? "Switching runtime to paper mode"
                : "Switching runtime to observation mode",
          };

    void executeLifecycleCommand({ kind: "set_mode", mode }, options);
  };

  const handleStrategyPathChange = (path: string) => {
    setStrategyViewModel((current) => ({
      ...current,
      selectedPath: path,
    }));
  };

  const handleSettingsReset = () => {
    if (!snapshot) {
      return;
    }

    const nextDraft = settingsDraftFromSnapshot(snapshot.settings);
    settingsDraftRef.current = nextDraft;
    setSettingsDraft(nextDraft);
    setSettingsDirty(false);
  };

  const handleStartWarmup = () => {
    void executeLifecycleCommand(
      { kind: "start_warmup" },
      { pendingLabel: "Starting warmup" },
    );
  };

  const handleArmToggle = () => {
    if (!snapshot) {
      return;
    }

    if (snapshot.status.arm_state === "armed") {
      void executeLifecycleCommand(
        { kind: "disarm" },
        { pendingLabel: "Disarming runtime" },
      );
      return;
    }

    const allowOverride = snapshot.readiness.report.hard_override_required;
    const confirmMessage = allowOverride
      ? "Arm now with a temporary hard override for this session?"
      : snapshot.status.mode === "live"
        ? "Arm LIVE trading? This enables live execution once commands or strategy logic fire."
        : "Arm the runtime for paper or observation execution?";

    void executeLifecycleCommand(
      { kind: "arm", allow_override: allowOverride },
      {
        pendingLabel: allowOverride
          ? "Arming runtime with temporary override"
          : "Arming runtime",
        confirmMessage,
      },
    );
  };

  const handlePauseResume = () => {
    if (!snapshot) {
      return;
    }

    void executeLifecycleCommand(
      { kind: snapshot.status.mode === "paused" ? "resume" : "pause" },
      {
        pendingLabel:
          snapshot.status.mode === "paused" ? "Resuming runtime" : "Pausing runtime",
      },
    );
  };

  const handleLoadSelectedStrategy = () => {
    void (async () => {
      const result = await executeLifecycleCommand(
        {
          kind: "load_strategy",
          path: strategyViewModel.selectedPath,
        },
        {
          pendingLabel: "Loading strategy through runtime host",
        },
      );

      if (result?.httpStatus === 200) {
        void refreshStrategyValidation(strategyViewModel.selectedPath);
      }
    })();
  };

  const handleManualEntrySubmit = () => {
    void (async () => {
      const result = await executeLifecycleCommand(
        {
          kind: "manual_entry",
          side: manualEntrySide,
          quantity: Number.parseInt(manualEntryQuantity, 10),
          tick_size: manualEntryTickSize.trim(),
          entry_reference_price: manualEntryReferencePrice.trim(),
          tick_value_usd: manualEntryTickValueUsd.trim() || null,
          reason: manualEntryReason.trim(),
        },
        {
          pendingLabel: `Submitting manual ${manualEntrySide} entry`,
          confirmMessage:
            "Submit a manual entry through the loaded strategy and runtime safety path now?",
        },
      );

      if (result?.httpStatus === 200) {
        setManualEntryReason("manual entry");
      }
    })();
  };

  const handleClosePositionSubmit = () => {
    void (async () => {
      const result = await executeLifecycleCommand(
        {
          kind: "close_position",
          contract_id: null,
          reason: closePositionReason.trim(),
        },
        {
          pendingLabel: "Flattening active broker position",
          confirmMessage:
            "Flatten the active broker position now? The runtime host will resolve the current contract from the synchronized broker snapshot and dispatch the audited flatten path.",
        },
      );

      if (result?.httpStatus === 200) {
        setClosePositionReason("flatten position");
      }
    })();
  };

  const handleCancelWorkingOrdersSubmit = () => {
    void (async () => {
      const result = await executeLifecycleCommand(
        {
          kind: "cancel_working_orders",
          reason: cancelWorkingOrdersReason.trim(),
        },
        {
          pendingLabel: "Cancelling working broker orders",
          confirmMessage: "Cancel all working broker orders for the loaded market now?",
        },
      );

      if (result?.httpStatus === 200) {
        setCancelWorkingOrdersReason("cancel working orders");
      }
    })();
  };

  return {
    strategyUploadInputRef,
    viewModel,
    strategyViewModel,
    eventFeed,
    commandFeedback,
    pendingAction,
    newEntriesReason,
    setNewEntriesReason,
    closePositionReason,
    setClosePositionReason,
    manualEntrySide,
    setManualEntrySide,
    manualEntryQuantity,
    setManualEntryQuantity,
    manualEntryTickSize,
    setManualEntryTickSize,
    manualEntryReferencePrice,
    setManualEntryReferencePrice,
    manualEntryTickValueUsd,
    setManualEntryTickValueUsd,
    manualEntryReason,
    setManualEntryReason,
    cancelWorkingOrdersReason,
    setCancelWorkingOrdersReason,
    reconnectReason,
    setReconnectReason,
    shutdownReason,
    setShutdownReason,
    selectedStrategyUploadFile,
    setSelectedStrategyUploadFile,
    settingsDraft,
    settingsDirty,
    refreshSnapshot,
    executeLifecycleCommand,
    executeReconnectDecision,
    executeShutdownDecision,
    updateNewEntriesEnabled,
    refreshStrategyLibrary,
    saveRuntimeSettings,
    refreshStrategyValidation,
    uploadSelectedStrategyFile,
    updateSettingsDraft,
    handleSetMode,
    handleStrategyPathChange,
    handleSettingsReset,
    handleStartWarmup,
    handleArmToggle,
    handlePauseResume,
    handleLoadSelectedStrategy,
    handleManualEntrySubmit,
    handleClosePositionSubmit,
    handleCancelWorkingOrdersSubmit,
  };
}
