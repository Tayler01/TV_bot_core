import { useRef, useState } from "react";

import { useDashboardRuntimeHost } from "./useDashboardRuntimeHost";
import { useDashboardSettingsWorkflow } from "./useDashboardSettingsWorkflow";
import { useDashboardStrategyWorkflow } from "./useDashboardStrategyWorkflow";
import type { RuntimeMode } from "../types/controlApi";

export function useDashboardController() {
  const strategyUploadInputRef = useRef<HTMLInputElement | null>(null);
  const runtimeHost = useDashboardRuntimeHost();
  const {
    viewModel,
    eventFeed,
    commandFeedback,
    setCommandFeedback,
    pendingAction,
    setPendingAction,
    refreshSnapshot,
    executeLifecycleCommand,
    updateSnapshot,
  } = runtimeHost;
  const snapshot = viewModel.snapshot;

  const {
    strategyViewModel,
    setStrategyViewModel,
    selectedStrategyUploadFile,
    setSelectedStrategyUploadFile,
    refreshStrategyLibrary,
    refreshStrategyValidation,
    uploadSelectedStrategyFile,
  } = useDashboardStrategyWorkflow({
    setPendingAction,
    setCommandFeedback,
    strategyUploadInputRef,
  });

  const {
    settingsDraft,
    settingsDirty,
    updateSettingsDraft,
    resetSettings,
    saveRuntimeSettings,
  } = useDashboardSettingsWorkflow({
    snapshot,
    setPendingAction,
    setCommandFeedback,
    updateSnapshot,
  });

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

  const executeReconnectDecision = async (
    decision: "close_position" | "leave_broker_protected" | "reattach_bot_management",
  ) => {
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
  };

  const executeShutdownDecision = async (decision: "flatten_first" | "leave_broker_protected") => {
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
  };

  const updateNewEntriesEnabled = async (enabled: boolean) => {
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
  };

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

  const handleStartWarmup = () => {
    void executeLifecycleCommand(
      { kind: "start_warmup" },
      { pendingLabel: "Starting warmup" },
    );
  };

  const handleSettingsReset = () => {
    resetSettings();
  };

  const handleArmToggle = () => {
    if (!snapshot) {
      return;
    }

    if (snapshot.status.arm_state === "armed") {
      void executeLifecycleCommand({ kind: "disarm" }, { pendingLabel: "Disarming runtime" });
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
