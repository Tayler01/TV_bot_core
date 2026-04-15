import type { RefObject } from "react";

import type { DashboardSnapshot } from "../lib/api";
import { reviewTone } from "../lib/dashboardPresentation";
import { formatInteger, formatMode } from "../lib/format";
import type {
  BannerTone,
  RuntimeSettingsDraft,
  StrategySummaryViewModel,
} from "../dashboardModels";
import type {
  RuntimeMode,
  RuntimeStrategyCatalogEntry,
  RuntimeStrategyValidationResponse,
} from "../types/controlApi";
import { ControlCluster, Definition, Panel, Pill } from "./dashboardPrimitives";

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

type ReconnectDecision =
  | "close_position"
  | "leave_broker_protected"
  | "reattach_bot_management";

type ShutdownDecision = "flatten_first" | "leave_broker_protected";

interface ControlCenterPanelProps {
  snapshot: DashboardSnapshot;
  pendingAction: string | null;
  strategyViewModel: StrategySummaryViewModel;
  selectedStrategyEntry: RuntimeStrategyCatalogEntry | null;
  selectedStrategyUploadFile: File | null;
  strategyUploadInputRef: RefObject<HTMLInputElement | null>;
  settingsDraft: RuntimeSettingsDraft | null;
  settingsDirty: boolean;
  newEntriesReason: string;
  closePositionReason: string;
  manualEntrySide: "buy" | "sell";
  manualEntryQuantity: string;
  manualEntryTickSize: string;
  manualEntryReferencePrice: string;
  manualEntryTickValueUsd: string;
  manualEntryReason: string;
  cancelWorkingOrdersReason: string;
  armButtonLabel: string;
  pauseButtonLabel: string;
  canLoadSelectedStrategy: boolean;
  canUploadSelectedStrategyFile: boolean;
  canDisableNewEntries: boolean;
  canEnableNewEntries: boolean;
  canSaveSettings: boolean;
  canManualEntry: boolean;
  canClosePosition: boolean;
  canCancelWorkingOrders: boolean;
  onSetMode: (mode: RuntimeMode) => void;
  onNewEntriesReasonChange: (value: string) => void;
  onSetNewEntriesEnabled: (enabled: boolean) => void;
  onStrategyPathChange: (path: string) => void;
  onStrategyUploadFileChange: (file: File | null) => void;
  onUploadSelectedStrategyFile: () => void;
  onRefreshStrategyLibrary: () => void;
  onRefreshStrategyValidation: () => void;
  onLoadSelectedStrategy: () => void;
  onSettingsStartupModeChange: (mode: RuntimeMode) => void;
  onSettingsDefaultStrategyPathChange: (value: string) => void;
  onSettingsAllowSqliteFallbackChange: (enabled: boolean) => void;
  onSettingsPaperAccountNameChange: (value: string) => void;
  onSettingsLiveAccountNameChange: (value: string) => void;
  onSaveRuntimeSettings: () => void;
  onResetSettings: () => void;
  onStartWarmup: () => void;
  onArmToggle: () => void;
  onPauseResume: () => void;
  onManualEntrySideChange: (side: "buy" | "sell") => void;
  onManualEntryQuantityChange: (value: string) => void;
  onManualEntryTickSizeChange: (value: string) => void;
  onManualEntryReferencePriceChange: (value: string) => void;
  onManualEntryTickValueUsdChange: (value: string) => void;
  onManualEntryReasonChange: (value: string) => void;
  onManualEntrySubmit: () => void;
  onClosePositionReasonChange: (value: string) => void;
  onClosePositionSubmit: () => void;
  onCancelWorkingOrdersReasonChange: (value: string) => void;
  onCancelWorkingOrdersSubmit: () => void;
}

export function ControlCenterPanel({
  snapshot,
  pendingAction,
  strategyViewModel,
  selectedStrategyEntry,
  selectedStrategyUploadFile,
  strategyUploadInputRef,
  settingsDraft,
  settingsDirty,
  newEntriesReason,
  closePositionReason,
  manualEntrySide,
  manualEntryQuantity,
  manualEntryTickSize,
  manualEntryReferencePrice,
  manualEntryTickValueUsd,
  manualEntryReason,
  cancelWorkingOrdersReason,
  armButtonLabel,
  pauseButtonLabel,
  canLoadSelectedStrategy,
  canUploadSelectedStrategyFile,
  canDisableNewEntries,
  canEnableNewEntries,
  canSaveSettings,
  canManualEntry,
  canClosePosition,
  canCancelWorkingOrders,
  onSetMode,
  onNewEntriesReasonChange,
  onSetNewEntriesEnabled,
  onStrategyPathChange,
  onStrategyUploadFileChange,
  onUploadSelectedStrategyFile,
  onRefreshStrategyLibrary,
  onRefreshStrategyValidation,
  onLoadSelectedStrategy,
  onSettingsStartupModeChange,
  onSettingsDefaultStrategyPathChange,
  onSettingsAllowSqliteFallbackChange,
  onSettingsPaperAccountNameChange,
  onSettingsLiveAccountNameChange,
  onSaveRuntimeSettings,
  onResetSettings,
  onStartWarmup,
  onArmToggle,
  onPauseResume,
  onManualEntrySideChange,
  onManualEntryQuantityChange,
  onManualEntryTickSizeChange,
  onManualEntryReferencePriceChange,
  onManualEntryTickValueUsdChange,
  onManualEntryReasonChange,
  onManualEntrySubmit,
  onClosePositionReasonChange,
  onClosePositionSubmit,
  onCancelWorkingOrdersReasonChange,
  onCancelWorkingOrdersSubmit,
}: ControlCenterPanelProps) {
  return (
    <Panel
      className="panel--full panel--command-center"
      eyebrow="Control Center"
      title="Lifecycle commands through /runtime/commands"
      detail={`Current mode: ${formatMode(snapshot.status.mode)} | Dispatch: ${snapshot.status.command_dispatch_detail}`}
    >
      <div className="control-shell">
        <ControlCluster
          eyebrow="Mode and gating"
          title="Runtime posture and operator entry controls"
          detail="High-frequency controls for mode selection and fresh-entry gating stay grouped together."
        >
          <div className="control-grid">
            <section className="control-card control-card--span-4">
              <p className="control-card__title">Mode</p>
              <div className="action-row">
                <button
                  className="command-button"
                  type="button"
                  disabled={pendingAction !== null || snapshot.status.mode === "paper"}
                  onClick={() => {
                    onSetMode("paper");
                  }}
                >
                  Paper
                </button>
                <button
                  className="command-button"
                  type="button"
                  disabled={pendingAction !== null || snapshot.status.mode === "observation"}
                  onClick={() => {
                    onSetMode("observation");
                  }}
                >
                  Observation
                </button>
                <button
                  className="command-button command-button--danger"
                  type="button"
                  disabled={pendingAction !== null || snapshot.status.mode === "live"}
                  onClick={() => {
                    onSetMode("live");
                  }}
                >
                  Live
                </button>
              </div>
            </section>

            <section className="control-card control-card--span-8">
              <p className="control-card__title">New entry gate</p>
              <div className="pill-row">
                <Pill
                  label={
                    snapshot.status.operator_new_entries_enabled
                      ? "New entries enabled"
                      : "New entries disabled"
                  }
                  tone={snapshot.status.operator_new_entries_enabled ? "healthy" : "warning"}
                />
                <Pill
                  label={
                    snapshot.status.operator_new_entries_reason ??
                    "Operator gate is open for fresh entries"
                  }
                  tone={snapshot.status.operator_new_entries_enabled ? "info" : "warning"}
                />
              </div>
              <label className="field field--wide">
                <span>Reason</span>
                <input
                  aria-label="New entry gate reason"
                  placeholder="operator gate"
                  value={newEntriesReason}
                  onChange={(event) => {
                    onNewEntriesReasonChange(event.target.value);
                  }}
                />
              </label>
              <div className="action-row">
                <button
                  className="command-button command-button--danger"
                  type="button"
                  disabled={!canDisableNewEntries}
                  onClick={() => {
                    onSetNewEntriesEnabled(false);
                  }}
                >
                  Disable new entries
                </button>
                <button
                  className="command-button"
                  type="button"
                  disabled={!canEnableNewEntries}
                  onClick={() => {
                    onSetNewEntriesEnabled(true);
                  }}
                >
                  Enable new entries
                </button>
              </div>
              <p className="control-card__note">
                This gate blocks fresh entry requests through the runtime host while still leaving
                flatten, close, and cancel actions available on existing exposure.
              </p>
            </section>
          </div>
        </ControlCluster>

        <ControlCluster
          eyebrow="Strategy and settings"
          title="Library workflow and runtime configuration"
          detail="Strategy selection, upload, validation, and settings edits stay backend-owned."
        >
          <div className="control-grid">
            <section className="control-card control-card--span-7">
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
                      onStrategyPathChange(event.target.value);
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
                <label className="field field--wide">
                  <span>Upload strategy file</span>
                  <input
                    ref={strategyUploadInputRef}
                    aria-label="Upload strategy file"
                    type="file"
                    accept=".md,text/markdown"
                    disabled={pendingAction !== null}
                    onChange={(event) => {
                      onStrategyUploadFileChange(event.target.files?.[0] ?? null);
                    }}
                  />
                </label>
                <div className="action-row">
                  <button
                    className="command-button"
                    type="button"
                    disabled={!canUploadSelectedStrategyFile}
                    onClick={onUploadSelectedStrategyFile}
                  >
                    Upload to library
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={strategyViewModel.libraryState === "loading"}
                    onClick={onRefreshStrategyLibrary}
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
                    onClick={onRefreshStrategyValidation}
                  >
                    Validate selection
                  </button>
                  <button
                    className="command-button"
                    type="button"
                    disabled={!canLoadSelectedStrategy}
                    onClick={onLoadSelectedStrategy}
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
                <Definition label="Selected" value={strategyLabel(strategyViewModel.validation)} />
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
                    snapshot.status.current_strategy?.path === strategyViewModel.selectedPath
                      ? "Loaded into runtime"
                      : "Not loaded"
                  }
                />
                <Definition
                  label="Upload ready"
                  value={
                    selectedStrategyUploadFile
                      ? selectedStrategyUploadFile.name
                      : "Choose a local Markdown strategy file"
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
                    <li key={`${issue.message}-${index}`}>{issue.message}</li>
                  ))}
                </ul>
              ) : null}
              {strategyViewModel.validation?.warnings.length ? (
                <ul className="issue-list issue-list--warning">
                  {strategyViewModel.validation.warnings.slice(0, 3).map((issue, index) => (
                    <li key={`${issue.message}-${index}`}>{issue.message}</li>
                  ))}
                </ul>
              ) : null}
              <p className="control-card__note">
                The dashboard now uploads, browses, validates, and loads strategy Markdown only
                through the local runtime host, keeping file writes and validation inside the
                backend-owned strategy library workflow.
              </p>
            </section>

            <section className="control-card control-card--span-5">
              <p className="control-card__title">Runtime settings</p>
              <div className="pill-row">
                <Pill
                  label={
                    snapshot.settings.persistence_mode === "config_file"
                      ? "Config file backed"
                      : "Session only"
                  }
                  tone={
                    snapshot.settings.persistence_mode === "config_file" ? "healthy" : "warning"
                  }
                />
                <Pill
                  label={snapshot.settings.restart_required ? "Restart required" : "Live applied"}
                  tone={snapshot.settings.restart_required ? "warning" : "healthy"}
                />
                <Pill
                  label={snapshot.settings.config_file_path ?? "No config file path"}
                  tone={snapshot.settings.config_file_path ? "info" : "warning"}
                />
              </div>
              <div className="control-grid">
                <label className="field">
                  <span>Startup mode</span>
                  <select
                    aria-label="Runtime startup mode"
                    value={settingsDraft?.startupMode ?? snapshot.settings.editable.startup_mode}
                    disabled={pendingAction !== null}
                    onChange={(event) => {
                      onSettingsStartupModeChange(event.target.value as RuntimeMode);
                    }}
                  >
                    <option value="paper">Paper</option>
                    <option value="observation">Observation</option>
                    <option value="paused">Paused</option>
                    <option value="live">Live</option>
                  </select>
                </label>
                <label className="field field--wide">
                  <span>Default strategy path</span>
                  <input
                    aria-label="Default strategy path"
                    placeholder="strategies/examples/gc_momentum_fade_v1.md"
                    value={
                      settingsDraft?.defaultStrategyPath ??
                      (snapshot.settings.editable.default_strategy_path ?? "")
                    }
                    disabled={pendingAction !== null}
                    onChange={(event) => {
                      onSettingsDefaultStrategyPathChange(event.target.value);
                    }}
                  />
                </label>
                <label className="field">
                  <span>Persistence fallback</span>
                  <select
                    aria-label="Persistence fallback policy"
                    value={
                      (settingsDraft?.allowSqliteFallback ??
                      snapshot.settings.editable.allow_sqlite_fallback)
                        ? "allow"
                        : "block"
                    }
                    disabled={pendingAction !== null}
                    onChange={(event) => {
                      onSettingsAllowSqliteFallbackChange(event.target.value === "allow");
                    }}
                  >
                    <option value="block">Require primary Postgres</option>
                    <option value="allow">Allow SQLite fallback</option>
                  </select>
                </label>
                <label className="field">
                  <span>Paper account name</span>
                  <input
                    aria-label="Paper account name"
                    placeholder="paper-primary"
                    value={
                      settingsDraft?.paperAccountName ??
                      (snapshot.settings.editable.paper_account_name ?? "")
                    }
                    disabled={pendingAction !== null}
                    onChange={(event) => {
                      onSettingsPaperAccountNameChange(event.target.value);
                    }}
                  />
                </label>
                <label className="field">
                  <span>Live account name</span>
                  <input
                    aria-label="Live account name"
                    placeholder="live-primary"
                    value={
                      settingsDraft?.liveAccountName ??
                      (snapshot.settings.editable.live_account_name ?? "")
                    }
                    disabled={pendingAction !== null}
                    onChange={(event) => {
                      onSettingsLiveAccountNameChange(event.target.value);
                    }}
                  />
                </label>
              </div>
              <div className="action-row">
                <button
                  className="command-button"
                  type="button"
                  disabled={!canSaveSettings}
                  onClick={onSaveRuntimeSettings}
                >
                  Save runtime settings
                </button>
                <button
                  className="command-button"
                  type="button"
                  disabled={!settingsDirty || pendingAction !== null}
                  onClick={onResetSettings}
                >
                  Reset form
                </button>
              </div>
              <dl className="definition-list">
                <Definition label="HTTP bind" value={snapshot.settings.http_bind} />
                <Definition label="WebSocket bind" value={snapshot.settings.websocket_bind} />
                <Definition
                  label="Config path"
                  value={snapshot.settings.config_file_path ?? "Runtime launched without a config file"}
                />
                <Definition
                  label="Effective path"
                  value={
                    snapshot.settings.editable.default_strategy_path ?? "No default strategy path"
                  }
                />
              </dl>
              <p className="control-card__note">{snapshot.settings.detail}</p>
            </section>
          </div>
        </ControlCluster>

        <ControlCluster
          eyebrow="Execution controls"
          title="Warmup, arming, and manual operator actions"
          detail="Execution-facing controls stay separate from strategy and settings work."
        >
          <div className="control-grid">
            <section className="control-card control-card--span-4">
              <p className="control-card__title">Warmup</p>
              <div className="action-row">
                <button
                  className="command-button"
                  type="button"
                  disabled={pendingAction !== null || !snapshot.status.strategy_loaded}
                  onClick={onStartWarmup}
                >
                  Start warmup
                </button>
              </div>
              <p className="control-card__note">
                Strategy loaded: {snapshot.status.strategy_loaded ? "Yes" : "No"} | Warmup:{" "}
                {formatMode(snapshot.status.warmup_status)}
              </p>
            </section>

            <section className="control-card control-card--span-4">
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
                  onClick={onArmToggle}
                >
                  {armButtonLabel}
                </button>
              </div>
              <p className="control-card__note">
                Arm state: {formatMode(snapshot.status.arm_state)} | Override required:{" "}
                {snapshot.readiness.report.hard_override_required ? "Yes" : "No"}
              </p>
            </section>

            <section className="control-card control-card--span-4">
              <p className="control-card__title">Flow Control</p>
              <div className="action-row">
                <button
                  className="command-button"
                  type="button"
                  disabled={pendingAction !== null}
                  onClick={onPauseResume}
                >
                  {pauseButtonLabel}
                </button>
              </div>
              <p className="control-card__note">
                Use pause to stop new entries without changing the selected trading mode.
              </p>
            </section>

            <section className="control-card control-card--span-12">
              <p className="control-card__title">Operator actions</p>
              <div className="control-grid">
                <section className="control-card control-card--span-7">
                  <p className="control-card__title">Manual entry</p>
                  <form
                    className="flatten-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      if (!canManualEntry) {
                        return;
                      }

                      onManualEntrySubmit();
                    }}
                  >
                    <div className="control-grid">
                      <label className="field">
                        <span>Side</span>
                        <select
                          aria-label="Manual entry side"
                          value={manualEntrySide}
                          onChange={(event) => {
                            onManualEntrySideChange(event.target.value as "buy" | "sell");
                          }}
                        >
                          <option value="buy">Buy</option>
                          <option value="sell">Sell</option>
                        </select>
                      </label>
                      <label className="field">
                        <span>Quantity</span>
                        <input
                          aria-label="Manual entry quantity"
                          inputMode="numeric"
                          value={manualEntryQuantity}
                          onChange={(event) => {
                            onManualEntryQuantityChange(event.target.value);
                          }}
                        />
                      </label>
                      <label className="field">
                        <span>Tick size</span>
                        <input
                          aria-label="Manual entry tick size"
                          inputMode="decimal"
                          placeholder="0.25"
                          value={manualEntryTickSize}
                          onChange={(event) => {
                            onManualEntryTickSizeChange(event.target.value);
                          }}
                        />
                      </label>
                      <label className="field">
                        <span>Reference price</span>
                        <input
                          aria-label="Manual entry reference price"
                          inputMode="decimal"
                          placeholder="2410.50"
                          value={manualEntryReferencePrice}
                          onChange={(event) => {
                            onManualEntryReferencePriceChange(event.target.value);
                          }}
                        />
                      </label>
                      <label className="field">
                        <span>Tick value USD</span>
                        <input
                          aria-label="Manual entry tick value"
                          inputMode="decimal"
                          placeholder="Optional for risk-based sizing"
                          value={manualEntryTickValueUsd}
                          onChange={(event) => {
                            onManualEntryTickValueUsdChange(event.target.value);
                          }}
                        />
                      </label>
                    </div>
                    <label className="field field--wide">
                      <span>Reason</span>
                      <input
                        aria-label="Manual entry reason"
                        placeholder="manual entry"
                        value={manualEntryReason}
                        onChange={(event) => {
                          onManualEntryReasonChange(event.target.value);
                        }}
                      />
                    </label>
                    <button
                      className="command-button"
                      type="submit"
                      disabled={pendingAction !== null || !canManualEntry}
                    >
                      Submit manual entry
                    </button>
                  </form>
                  <p className="control-card__note">
                    Manual entry reuses the loaded strategy for order type, reversal, and
                    broker-protection handling. Reference price and tick inputs keep the execution
                    path explicit and strategy-agnostic.
                  </p>
                </section>

                <section className="control-card control-card--span-5">
                  <p className="control-card__title">Flatten current position</p>
                  <form
                    className="flatten-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      if (!canClosePosition) {
                        return;
                      }

                      onClosePositionSubmit();
                    }}
                  >
                    <label className="field field--wide">
                      <span>Reason</span>
                      <input
                        aria-label="Flatten position reason"
                        placeholder="flatten position"
                        value={closePositionReason}
                        onChange={(event) => {
                          onClosePositionReasonChange(event.target.value);
                        }}
                      />
                    </label>
                    <button
                      className="command-button command-button--danger"
                      type="submit"
                      disabled={pendingAction !== null || !canClosePosition}
                    >
                      Flatten current position
                    </button>
                  </form>
                  <p className="control-card__note">
                    This is the direct dashboard flatten control. The runtime host resolves the
                    active broker contract from the synchronized snapshot and keeps the action on
                    the same audited close/flatten path used elsewhere.
                  </p>
                </section>

                <section className="control-card control-card--span-5">
                  <p className="control-card__title">Cancel working orders</p>
                  <form
                    className="flatten-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      if (!canCancelWorkingOrders) {
                        return;
                      }

                      onCancelWorkingOrdersSubmit();
                    }}
                  >
                    <label className="field field--wide">
                      <span>Reason</span>
                      <input
                        aria-label="Cancel working orders reason"
                        placeholder="cancel working orders"
                        value={cancelWorkingOrdersReason}
                        onChange={(event) => {
                          onCancelWorkingOrdersReasonChange(event.target.value);
                        }}
                      />
                    </label>
                    <button
                      className="command-button"
                      type="submit"
                      disabled={pendingAction !== null || !canCancelWorkingOrders}
                    >
                      Cancel working orders
                    </button>
                  </form>
                  <p className="control-card__note">
                    Cancel routes only the current market&apos;s working orders through the audited
                    backend path.
                  </p>
                </section>
              </div>
              <p className="control-card__note">
                All three actions stay inside the local runtime host. Manual entry uses the loaded
                strategy and synchronized market contract, close resolves the active contract
                automatically when there is a single live position, and cancel routes only the
                current market&apos;s working orders through the audited backend path.
              </p>
            </section>
          </div>
        </ControlCluster>
      </div>
    </Panel>
  );
}

interface SafetyPanelProps {
  snapshot: DashboardSnapshot;
  reconnectReason: string;
  shutdownReason: string;
  reviewActionsDisabled: boolean;
  reconnectCloseDisabled: boolean;
  shutdownLeaveDisabled: boolean;
  shutdownFlattenDisabled: boolean;
  onReconnectReasonChange: (value: string) => void;
  onShutdownReasonChange: (value: string) => void;
  onReconnectDecision: (decision: ReconnectDecision) => void;
  onShutdownDecision: (decision: ShutdownDecision) => void;
}

export function SafetyPanel({
  snapshot,
  reconnectReason,
  shutdownReason,
  reviewActionsDisabled,
  reconnectCloseDisabled,
  shutdownLeaveDisabled,
  shutdownFlattenDisabled,
  onReconnectReasonChange,
  onShutdownReasonChange,
  onReconnectDecision,
  onShutdownDecision,
}: SafetyPanelProps) {
  return (
    <Panel eyebrow="Safety" title="Reconnect, shutdown, and operator guardrails">
      <div className="pill-row">
        <Pill
          label={
            snapshot.status.reconnect_review.required ? "Reconnect review active" : "Reconnect clear"
          }
          tone={reviewTone(snapshot.status.reconnect_review)}
        />
        <Pill
          label={
            snapshot.status.shutdown_review.blocked || snapshot.status.shutdown_review.awaiting_flatten
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
      {snapshot.status.reconnect_review.required ? (
        <section className="review-card">
          <p className="control-card__title">Reconnect review actions</p>
          <label className="field field--wide">
            <span>Reason</span>
            <input
              aria-label="Reconnect review reason"
              placeholder="resolve reconnect review"
              value={reconnectReason}
              onChange={(event) => {
                onReconnectReasonChange(event.target.value);
              }}
            />
          </label>
          <div className="action-row">
            <button
              className="command-button"
              type="button"
              disabled={reviewActionsDisabled}
              onClick={() => {
                onReconnectDecision("reattach_bot_management");
              }}
            >
              Reattach bot management
            </button>
            <button
              className="command-button"
              type="button"
              disabled={reviewActionsDisabled}
              onClick={() => {
                onReconnectDecision("leave_broker_protected");
              }}
            >
              Leave broker-side
            </button>
            <button
              className="command-button command-button--danger"
              type="button"
              disabled={reconnectCloseDisabled}
              onClick={() => {
                onReconnectDecision("close_position");
              }}
            >
              Close position
            </button>
          </div>
          <p className="control-card__note">
            The runtime host resolves the active contract id when there is only one open broker
            position, so reconnect-close can stay inside the audited control path.
          </p>
        </section>
      ) : null}
      {snapshot.status.shutdown_review.blocked ? (
        <section className="review-card">
          <p className="control-card__title">Shutdown review actions</p>
          <label className="field field--wide">
            <span>Reason</span>
            <input
              aria-label="Shutdown review reason"
              placeholder="resolve shutdown review"
              value={shutdownReason}
              onChange={(event) => {
                onShutdownReasonChange(event.target.value);
              }}
            />
          </label>
          <div className="action-row">
            <button
              className="command-button command-button--danger"
              type="button"
              disabled={shutdownFlattenDisabled}
              onClick={() => {
                onShutdownDecision("flatten_first");
              }}
            >
              Flatten first
            </button>
            <button
              className="command-button"
              type="button"
              disabled={shutdownLeaveDisabled}
              onClick={() => {
                onShutdownDecision("leave_broker_protected");
              }}
            >
              Leave broker-protected
            </button>
          </div>
          <p className="control-card__note">
            Leave-in-place is only enabled when every open position reports broker-side protection
            through the runtime host snapshot.
          </p>
        </section>
      ) : null}
      <p className="panel__footnote">
        Reconnect hardening now covers startup and reconnect review decisions through the real
        runtime host. The remaining work here is final operator polish and hands-on release
        verification.
      </p>
    </Panel>
  );
}
