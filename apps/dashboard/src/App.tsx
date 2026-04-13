import {
  startTransition,
  useEffect,
  useEffectEvent,
  useState,
  type ReactNode,
} from "react";

import {
  loadDashboardSnapshot,
  loadStrategyLibrary,
  sendLifecycleCommand,
  validateStrategyPath,
  type DashboardSnapshot,
  type LifecycleCommandResult,
} from "./lib/api";
import {
  formatCurrency,
  formatDateTime,
  formatDecimal,
  formatInteger,
  formatLatency,
  formatMode,
  formatSignedCurrency,
} from "./lib/format";
import type {
  ReadinessCheckStatus,
  RuntimeLifecycleCommand,
  RuntimeLifecycleResponse,
  RuntimeMode,
  RuntimeReconnectReviewStatus,
  RuntimeShutdownReviewStatus,
  RuntimeStatusSnapshot,
  RuntimeStrategyCatalogEntry,
  RuntimeStrategyLibraryResponse,
  RuntimeStrategyValidationResponse,
} from "./types/controlApi";

const REFRESH_INTERVAL_MS = 5_000;

type LoadState = "idle" | "loading" | "ready" | "error";
type BannerTone = "healthy" | "warning" | "danger" | "info";

interface ViewModel {
  snapshot: DashboardSnapshot | null;
  loadState: LoadState;
  error: string | null;
  lastAttemptedAt: string | null;
}

interface CommandFeedback {
  tone: BannerTone;
  message: string;
}

interface CommandOptions {
  confirmMessage?: string;
  pendingLabel: string;
}

interface StrategySummaryViewModel {
  library: RuntimeStrategyLibraryResponse | null;
  validation: RuntimeStrategyValidationResponse | null;
  libraryError: string | null;
  validationError: string | null;
  libraryState: LoadState;
  validationState: LoadState;
  selectedPath: string;
}

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

function modeTone(mode: RuntimeMode): "paper" | "live" | "neutral" {
  switch (mode) {
    case "paper":
      return "paper";
    case "live":
      return "live";
    default:
      return "neutral";
  }
}

function statusTone(status: ReadinessCheckStatus | BannerTone) {
  switch (status) {
    case "pass":
    case "healthy":
      return "healthy";
    case "warning":
      return "warning";
    case "blocking":
    case "danger":
      return "danger";
    default:
      return "info";
  }
}

function feedbackToneFromHttpStatus(httpStatus: number): BannerTone {
  if (httpStatus >= 500) {
    return "danger";
  }

  if (httpStatus === 409 || httpStatus === 428) {
    return "warning";
  }

  return "healthy";
}

function humanMemory(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "Unavailable";
  }

  const gibibytes = value / 1024 / 1024 / 1024;
  return `${gibibytes.toFixed(2)} GiB`;
}

function reviewTone(review: RuntimeReconnectReviewStatus | RuntimeShutdownReviewStatus) {
  if ("required" in review) {
    return review.required ? "warning" : "healthy";
  }

  return review.blocked || review.awaiting_flatten ? "warning" : "healthy";
}

function latestLatency(status: RuntimeStatusSnapshot) {
  return status.latest_trade_latency?.latency.end_to_end_fill_latency_ms ?? null;
}

function reviewSummary(status: RuntimeStatusSnapshot) {
  if (status.reconnect_review.required) {
    return "Reconnect review required";
  }

  if (status.shutdown_review.blocked || status.shutdown_review.awaiting_flatten) {
    return "Shutdown review pending";
  }

  return "No active safety review";
}

function mergeLifecycleResponseIntoSnapshot(
  snapshot: DashboardSnapshot | null,
  response: RuntimeLifecycleResponse,
): DashboardSnapshot | null {
  if (!snapshot) {
    return null;
  }

  return {
    ...snapshot,
    fetchedAt: new Date().toISOString(),
    status: response.status,
    readiness: response.readiness,
  };
}

function selectStrategyPath(
  library: RuntimeStrategyLibraryResponse | null,
  currentPath: string,
): string {
  if (!library || library.strategies.length === 0) {
    return "";
  }

  if (library.strategies.some((entry) => entry.path === currentPath)) {
    return currentPath;
  }

  return library.strategies.find((entry) => entry.valid)?.path ?? library.strategies[0]?.path ?? "";
}

function strategyTone(entry: RuntimeStrategyCatalogEntry | null | undefined): BannerTone {
  if (!entry) {
    return "info";
  }

  if (!entry.valid) {
    return "danger";
  }

  if (entry.warning_count > 0) {
    return "warning";
  }

  return "healthy";
}

function validationTone(validation: RuntimeStrategyValidationResponse | null): BannerTone {
  if (!validation) {
    return "info";
  }

  if (!validation.valid) {
    return "danger";
  }

  if (validation.warnings.length > 0) {
    return "warning";
  }

  return "healthy";
}

function strategyLabel(validation: RuntimeStrategyValidationResponse | null): string {
  if (!validation) {
    return "No strategy selected";
  }

  if (validation.summary) {
    return `${validation.summary.name} v${validation.summary.version}`;
  }

  return validation.title ?? validation.display_path;
}

function Panel({
  eyebrow,
  title,
  detail,
  children,
  className,
}: {
  eyebrow: string;
  title: string;
  detail?: string;
  children: ReactNode;
  className?: string;
}) {
  const panelClassName = className ? `panel ${className}` : "panel";

  return (
    <section className={panelClassName}>
      <div className="panel__heading">
        <div>
          <p className="eyebrow">{eyebrow}</p>
          <h2>{title}</h2>
        </div>
        {detail ? <p className="panel__detail">{detail}</p> : null}
      </div>
      {children}
    </section>
  );
}

function Pill({
  label,
  tone,
}: {
  label: string;
  tone: "healthy" | "warning" | "danger" | "info";
}) {
  return <span className={`pill pill--${tone}`}>{label}</span>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function MiniMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="mini-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function Definition({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}

function App() {
  const [viewModel, setViewModel] = useState<ViewModel>(INITIAL_VIEW_MODEL);
  const [strategyViewModel, setStrategyViewModel] = useState<StrategySummaryViewModel>(
    INITIAL_STRATEGY_VIEW_MODEL,
  );
  const [commandFeedback, setCommandFeedback] = useState<CommandFeedback | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [flattenContractId, setFlattenContractId] = useState("");
  const [flattenReason, setFlattenReason] = useState("dashboard flatten request");

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
            refreshedSnapshot ?? mergeLifecycleResponseIntoSnapshot(current.snapshot, result.response),
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
  const selectedStrategyEntry =
    strategyViewModel.library?.strategies.find(
      (entry) => entry.path === strategyViewModel.selectedPath,
    ) ?? null;
  const headlineTone = snapshot ? modeTone(snapshot.status.mode) : "neutral";
  const readinessCounts = snapshot
    ? snapshot.readiness.report.checks.reduce(
        (counts, check) => {
          counts[check.status] += 1;
          return counts;
        },
        { pass: 0, warning: 0, blocking: 0 },
      )
    : { pass: 0, warning: 0, blocking: 0 };
  const armButtonLabel = snapshot
    ? snapshot.status.arm_state === "armed"
      ? "Disarm runtime"
      : snapshot.readiness.report.hard_override_required
        ? "Arm with temporary override"
        : "Arm runtime"
    : "Arm runtime";
  const pauseButtonLabel = snapshot?.status.mode === "paused" ? "Resume runtime" : "Pause runtime";
  const flattenContractIdValue = Number.parseInt(flattenContractId, 10);
  const canSubmitFlatten =
    Number.isInteger(flattenContractIdValue) &&
    flattenContractIdValue > 0 &&
    flattenReason.trim().length > 0 &&
    snapshot?.status.command_dispatch_ready === true;
  const canLoadSelectedStrategy =
    strategyViewModel.selectedPath.length > 0 &&
    strategyViewModel.validation?.valid === true &&
    pendingAction === null;

  return (
    <main className="shell">
      <div className={`hero hero--${headlineTone}`}>
        <div className="hero__copy">
          <p className="eyebrow">TV Bot Control Center</p>
          <h1>Operator dashboard for the local runtime host</h1>
          <p className="hero__summary">
            This slice adds the first real control-center actions on top of the local control
            plane, while keeping live and paper modes visually distinct and confirming the risky
            paths before the dashboard sends them.
          </p>
        </div>
        <div className="hero__meta">
          <div className="hero__mode-lockup">
            <span className="hero__mode-label">Mode</span>
            <strong>{snapshot ? formatMode(snapshot.status.mode) : "Waiting for runtime"}</strong>
          </div>
          <div className="hero__actions">
            <button
              className="refresh-button"
              type="button"
              onClick={() => {
                void refreshSnapshot();
              }}
            >
              Refresh now
            </button>
            <p className="hero__timestamp">
              Last sync{" "}
              {snapshot ? formatDateTime(snapshot.fetchedAt) : formatDateTime(viewModel.lastAttemptedAt)}
            </p>
          </div>
        </div>
      </div>

      {viewModel.error ? (
        <section className="banner banner--warning" role="status">
          <strong>Local control-plane read failed.</strong>
          <span>{viewModel.error}</span>
        </section>
      ) : null}

      {commandFeedback ? (
        <section className={`banner banner--${commandFeedback.tone}`} role="status">
          <strong>Runtime command result.</strong>
          <span>{commandFeedback.message}</span>
        </section>
      ) : null}

      {pendingAction ? (
        <section className="banner banner--info" role="status">
          <strong>Command in progress.</strong>
          <span>{pendingAction}</span>
        </section>
      ) : null}

      {!snapshot && viewModel.loadState !== "error" ? (
        <section className="empty-state" aria-live="polite">
          <h2>Waiting for runtime status</h2>
          <p>The dashboard is polling the local runtime host for its first snapshot.</p>
        </section>
      ) : null}

      {snapshot ? (
        <div className="dashboard-grid">
          <Panel
            className="panel--full"
            eyebrow="Control Center"
            title="Lifecycle commands through /runtime/commands"
            detail={`Current mode: ${formatMode(snapshot.status.mode)} | Dispatch: ${snapshot.status.command_dispatch_detail}`}
          >
            <div className="control-grid">
              <section className="control-card">
                <p className="control-card__title">Mode</p>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null || snapshot.status.mode === "paper"}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "set_mode", mode: "paper" },
                        { pendingLabel: "Switching runtime to paper mode" },
                      );
                    }}
                  >
                    Paper
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null || snapshot.status.mode === "observation"}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "set_mode", mode: "observation" },
                        { pendingLabel: "Switching runtime to observation mode" },
                      );
                    }}
                  >
                    Observation
                  </button>
                  <button
                    className="command-button command-button--danger"
                    type="button"
                    disabled={pendingAction !== null || snapshot.status.mode === "live"}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "set_mode", mode: "live" },
                        {
                          pendingLabel: "Switching runtime to live mode",
                          confirmMessage:
                            "Switch the runtime into LIVE mode? Paper and live are intentionally separated. Continue?",
                        },
                      );
                    }}
                  >
                    Live
                  </button>
                </div>
              </section>

              <section className="control-card control-card--wide">
                <p className="control-card__title">Strategy Library</p>
                <div className="strategy-toolbar">
                  <label className="field field--wide">
                    <span>Available strategy</span>
                    <select
                      aria-label="Available strategy"
                      value={strategyViewModel.selectedPath}
                      disabled={
                        strategyViewModel.libraryState === "loading" ||
                        !strategyViewModel.library?.strategies.length
                      }
                      onChange={(event) => {
                        setStrategyViewModel((current) => ({
                          ...current,
                          selectedPath: event.target.value,
                        }));
                      }}
                    >
                      {strategyViewModel.library?.strategies.length ? (
                        strategyViewModel.library.strategies.map((entry) => (
                          <option key={entry.path} value={entry.path}>
                            {entry.name ?? entry.title ?? entry.display_path}
                          </option>
                        ))
                      ) : (
                        <option value="">No strategies available</option>
                      )}
                    </select>
                  </label>
                  <div className="action-row">
                    <button
                      className="command-button"
                      type="button"
                      disabled={strategyViewModel.libraryState === "loading"}
                      onClick={() => {
                        void refreshStrategyLibrary();
                      }}
                    >
                      Refresh library
                    </button>
                    <button
                      className="command-button"
                      type="button"
                      disabled={
                        !strategyViewModel.selectedPath ||
                        strategyViewModel.validationState === "loading"
                      }
                      onClick={() => {
                        void refreshStrategyValidation(strategyViewModel.selectedPath);
                      }}
                    >
                      Validate selection
                    </button>
                    <button
                      className="command-button"
                      type="button"
                      disabled={!canLoadSelectedStrategy}
                      onClick={() => {
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
                      }}
                    >
                      Load selected strategy
                    </button>
                  </div>
                </div>
                <div className="pill-row">
                  <Pill
                    label={
                      selectedStrategyEntry
                        ? selectedStrategyEntry.valid
                          ? "Library entry valid"
                          : "Library entry needs fixes"
                        : "No strategy selected"
                    }
                    tone={strategyTone(selectedStrategyEntry)}
                  />
                  <Pill
                    label={
                      strategyViewModel.validation
                        ? strategyViewModel.validation.valid
                          ? "Validation passed"
                          : "Validation failed"
                        : strategyViewModel.validationState === "loading"
                          ? "Validation running"
                          : "Validation idle"
                    }
                    tone={
                      strategyViewModel.validationState === "loading"
                        ? "info"
                        : validationTone(strategyViewModel.validation)
                    }
                  />
                  <Pill
                    label={`${strategyViewModel.validation?.warnings.length ?? 0} warning(s)`}
                    tone={
                      (strategyViewModel.validation?.warnings.length ?? 0) > 0
                        ? "warning"
                        : "healthy"
                    }
                  />
                  <Pill
                    label={`${strategyViewModel.validation?.errors.length ?? 0} error(s)`}
                    tone={
                      (strategyViewModel.validation?.errors.length ?? 0) > 0
                        ? "danger"
                        : "healthy"
                    }
                  />
                </div>
                <dl className="definition-list">
                  <Definition
                    label="Selected"
                    value={strategyLabel(strategyViewModel.validation)}
                  />
                  <Definition
                    label="Path"
                    value={
                      strategyViewModel.validation?.display_path ??
                      selectedStrategyEntry?.display_path ??
                      "No strategy selected"
                    }
                  />
                  <Definition
                    label="Scanned roots"
                    value={
                      strategyViewModel.library?.scanned_roots.length
                        ? strategyViewModel.library.scanned_roots.join(" | ")
                        : "No strategy library roots detected"
                    }
                  />
                  <Definition
                    label="Load status"
                    value={
                      snapshot?.status.current_strategy?.path === strategyViewModel.selectedPath
                        ? "Loaded into runtime"
                        : "Not loaded"
                    }
                  />
                </dl>
                {strategyViewModel.libraryError ? (
                  <p className="control-card__note">{strategyViewModel.libraryError}</p>
                ) : null}
                {strategyViewModel.validationError ? (
                  <p className="control-card__note">{strategyViewModel.validationError}</p>
                ) : null}
                {strategyViewModel.validation?.errors.length ? (
                  <ul className="issue-list">
                    {strategyViewModel.validation.errors.slice(0, 3).map((issue, index) => (
                      <li key={`${issue.message}-${index}`}>
                        {issue.message}
                      </li>
                    ))}
                  </ul>
                ) : null}
                {strategyViewModel.validation?.warnings.length ? (
                  <ul className="issue-list issue-list--warning">
                    {strategyViewModel.validation.warnings.slice(0, 3).map((issue, index) => (
                      <li key={`${issue.message}-${index}`}>
                        {issue.message}
                      </li>
                    ))}
                  </ul>
                ) : null}
                <p className="control-card__note">
                  The dashboard now browses and validates strategy Markdown only through the local
                  runtime host, then loads the selected path through the existing audited lifecycle
                  command path.
                </p>
              </section>

              <section className="control-card">
                <p className="control-card__title">Warmup</p>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null || !snapshot.status.strategy_loaded}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: "start_warmup" },
                        { pendingLabel: "Starting warmup" },
                      );
                    }}
                  >
                    Start warmup
                  </button>
                </div>
                <p className="control-card__note">
                  Strategy loaded: {snapshot.status.strategy_loaded ? "Yes" : "No"} | Warmup:{" "}
                  {formatMode(snapshot.status.warmup_status)}
                </p>
              </section>

              <section className="control-card">
                <p className="control-card__title">Arming</p>
                <div className="action-row">
                  <button
                    className={
                      snapshot.status.arm_state === "armed"
                        ? "command-button"
                        : "command-button command-button--danger"
                    }
                    type="button"
                    disabled={pendingAction !== null}
                    onClick={() => {
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
                    }}
                  >
                    {armButtonLabel}
                  </button>
                </div>
                <p className="control-card__note">
                  Arm state: {formatMode(snapshot.status.arm_state)} | Override required:{" "}
                  {snapshot.readiness.report.hard_override_required ? "Yes" : "No"}
                </p>
              </section>

              <section className="control-card">
                <p className="control-card__title">Flow Control</p>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={pendingAction !== null}
                    onClick={() => {
                      void executeLifecycleCommand(
                        { kind: snapshot.status.mode === "paused" ? "resume" : "pause" },
                        {
                          pendingLabel:
                            snapshot.status.mode === "paused"
                              ? "Resuming runtime"
                              : "Pausing runtime",
                        },
                      );
                    }}
                  >
                    {pauseButtonLabel}
                  </button>
                </div>
                <p className="control-card__note">
                  Use pause to stop new entries without changing the selected trading mode.
                </p>
              </section>

              <section className="control-card control-card--wide">
                <p className="control-card__title">Flatten</p>
                <form
                  className="flatten-form"
                  onSubmit={(event) => {
                    event.preventDefault();
                    if (!canSubmitFlatten) {
                      return;
                    }

                    void (async () => {
                      const result = await executeLifecycleCommand(
                        {
                          kind: "flatten",
                          contract_id: flattenContractIdValue,
                          reason: flattenReason.trim(),
                        },
                        {
                          pendingLabel: `Flattening contract ${flattenContractIdValue}`,
                          confirmMessage: `Flatten contract ${flattenContractIdValue} now? Existing broker-managed exposure will be liquidated.`,
                        },
                      );

                      if (result && result.httpStatus === 200) {
                        setFlattenReason("dashboard flatten request");
                      }
                    })();
                  }}
                >
                  <label className="field">
                    <span>Active contract id</span>
                    <input
                      aria-label="Active contract id"
                      inputMode="numeric"
                      placeholder="4444"
                      value={flattenContractId}
                      onChange={(event) => {
                        setFlattenContractId(event.target.value);
                      }}
                    />
                  </label>
                  <label className="field field--wide">
                    <span>Reason</span>
                    <input
                      aria-label="Flatten reason"
                      placeholder="dashboard flatten request"
                      value={flattenReason}
                      onChange={(event) => {
                        setFlattenReason(event.target.value);
                      }}
                    />
                  </label>
                  <button
                    className="command-button command-button--danger"
                    type="submit"
                    disabled={pendingAction !== null || !canSubmitFlatten}
                  >
                    Flatten position
                  </button>
                </form>
                <p className="control-card__note">
                  The current overview does not project broker contract ids yet, so flatten takes
                  an explicit contract id until the richer order and position views land.
                </p>
              </section>
            </div>
          </Panel>

          <Panel
            eyebrow="Runtime"
            title={reviewSummary(snapshot.status)}
            detail={`HTTP ${snapshot.status.http_bind} | WS ${snapshot.status.websocket_bind}`}
          >
            <div className="metric-row">
              <Metric label="Arm state" value={formatMode(snapshot.status.arm_state)} />
              <Metric label="Warmup" value={formatMode(snapshot.status.warmup_status)} />
              <Metric
                label="Account"
                value={snapshot.status.current_account_name ?? "Not selected"}
              />
              <Metric
                label="Dispatch"
                value={snapshot.status.command_dispatch_ready ? "Ready" : "Blocked"}
              />
            </div>
            <div className="pill-row">
              <Pill label={formatMode(snapshot.status.mode)} tone={statusTone("info")} />
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
            </div>
          </Panel>

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
            </dl>
          </Panel>

          <Panel eyebrow="Latency" title="Latest trade-path timing">
            <div className="metric-row">
              <Metric
                label="Recorded paths"
                value={formatInteger(snapshot.status.recorded_trade_latency_count)}
              />
              <Metric
                label="End to end fill"
                value={formatLatency(latestLatency(snapshot.status))}
              />
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
            </dl>
          </Panel>

          <Panel eyebrow="Safety" title="Reconnect, shutdown, and operator guardrails">
            <div className="pill-row">
              <Pill
                label={
                  snapshot.status.reconnect_review.required
                    ? "Reconnect review active"
                    : "Reconnect clear"
                }
                tone={reviewTone(snapshot.status.reconnect_review)}
              />
              <Pill
                label={
                  snapshot.status.shutdown_review.blocked ||
                  snapshot.status.shutdown_review.awaiting_flatten
                    ? "Shutdown review active"
                    : "Shutdown clear"
                }
                tone={reviewTone(snapshot.status.shutdown_review)}
              />
            </div>
            <dl className="definition-list">
              <Definition
                label="Reconnect review"
                value={
                  snapshot.status.reconnect_review.reason ??
                  (snapshot.status.reconnect_review.last_decision
                    ? `Last decision: ${formatMode(snapshot.status.reconnect_review.last_decision)}`
                    : "No reconnect review pending")
                }
              />
              <Definition
                label="Shutdown review"
                value={
                  snapshot.status.shutdown_review.reason ??
                  (snapshot.status.shutdown_review.decision
                    ? `Last decision: ${formatMode(snapshot.status.shutdown_review.decision)}`
                    : "No shutdown review pending")
                }
              />
              <Definition
                label="Reconnect counts"
                value={formatInteger(
                  snapshot.status.broker_status?.reconnect_count ??
                    snapshot.health.system_health?.reconnect_count,
                )}
              />
            </dl>
            <p className="panel__footnote">
              Follow-up after this dashboard overview: circle back to reconnect hardening for the
              broader operator-resolution campaign and final paper-mode recovery polish.
            </p>
          </Panel>
        </div>
      ) : null}
    </main>
  );
}

export default App;
