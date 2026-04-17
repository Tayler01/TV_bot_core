import { useState } from "react";

import { latencyStages, reviewSummary } from "./lib/dashboardPresentation";
import {
  dispatchTone,
  isPositiveNumberInput,
  latestPnlSnapshot,
  modeTone,
  perTradePnlForProjection,
  pnlChartForProjection,
  pnlChartPath,
  readinessSummary,
  readinessTone,
  recentFillsForProjection,
  recentJournalRecords,
  recentTradeSummariesForProjection,
  reviewButtonDisabled,
  summarizeJournalRecords,
  summarizeRecentEvents,
  tradePerformanceForProjection,
  warmupTone,
  workingOrdersForProjection,
} from "./lib/dashboardProjection";
import {
  formatDateTime,
  formatMode,
  formatWarmupMode,
} from "./lib/format";
import {
  EventsPanel,
  HealthPanel,
  HistoryPanel,
  JournalPanel,
  LatencyPanel,
  ReadinessPanel,
  RuntimeSummaryPanel,
} from "./components/dashboardMonitoring";
import {
  ControlCenterPanel,
  SafetyPanel,
  StrategySetupPanel,
} from "./components/dashboardControlPanels";
import { LiveChartPanel } from "./components/dashboardLiveChart";
import { SignalTile } from "./components/dashboardPrimitives";
import type { LatencyStageViewModel } from "./dashboardModels";
import { useDashboardController } from "./hooks/useDashboardController";
import { useDashboardChart } from "./hooks/useDashboardChart";

type WorkspaceDockSection =
  | "health"
  | "history"
  | "latency"
  | "journal"
  | "events"
  | "setup";

const workspaceDockSections: ReadonlyArray<{
  section: WorkspaceDockSection;
  label: string;
}> = [
  { section: "history", label: "Trades" },
  { section: "setup", label: "Setup" },
  { section: "health", label: "Health" },
  { section: "latency", label: "Latency" },
  { section: "journal", label: "Journal" },
  { section: "events", label: "Events" },
];

function App() {
  const {
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
  } = useDashboardController();
  const {
    chartViewModel,
    setSelectedTimeframe,
    refreshChart,
    loadOlderHistory,
  } = useDashboardChart(viewModel.snapshot);
  const [activeDockSection, setActiveDockSection] =
    useState<WorkspaceDockSection>("history");

  const snapshot = viewModel.snapshot;
  const selectedStrategyEntry =
    strategyViewModel.library?.strategies.find(
      (entry) => entry.path === strategyViewModel.selectedPath,
    ) ?? null;
  const headlineTone = snapshot ? modeTone(snapshot.status.mode) : "neutral";
  const readinessCounts = snapshot
    ? snapshot.readiness.report.checks.reduce<{
        pass: number;
        warning: number;
        blocking: number;
      }>(
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
  const openWorkingOrders = snapshot ? workingOrdersForProjection(snapshot) : [];
  const recentFills = snapshot ? recentFillsForProjection(snapshot) : [];
  const recentTrades = snapshot ? recentTradeSummariesForProjection(snapshot) : [];
  const journalRecords = snapshot ? recentJournalRecords(snapshot) : [];
  const tradePerformance = snapshot ? tradePerformanceForProjection(snapshot) : null;
  const pnlChart = snapshot ? pnlChartForProjection(snapshot) : null;
  const pnlChartPathData = pnlChart ? pnlChartPath(pnlChart.points) : "";
  const perTradePnl = snapshot ? perTradePnlForProjection(snapshot) : [];
  const journalSummary = summarizeJournalRecords(journalRecords);
  const eventHeadlineSummary = summarizeRecentEvents(eventFeed.recentEvents);
  const latencyBreakdown = snapshot ? latencyStages(snapshot.health.latest_trade_latency) : [];
  const slowestLatencyStage = latencyBreakdown.reduce<LatencyStageViewModel | null>(
    (slowest, stage) => {
      if (stage.value === null) {
        return slowest;
      }

      if (!slowest || (slowest.value ?? -1) < stage.value) {
        return stage;
      }

      return slowest;
    },
    null,
  );
  const projectedPnlSnapshot = snapshot ? latestPnlSnapshot(snapshot) : null;
  const feedStatuses = snapshot?.status.market_data_status?.session.market_data.feed_statuses ?? [];
  const readinessState = readinessSummary(readinessCounts);
  const activeReviewSummary = snapshot ? reviewSummary(snapshot.status) : "Awaiting runtime";
  const canManualEntry =
    snapshot != null &&
    snapshot.status.strategy_loaded === true &&
    snapshot.status.command_dispatch_ready === true &&
    snapshot.status.operator_new_entries_enabled === true &&
    snapshot.status.arm_state === "armed" &&
    (snapshot.status.mode === "paper" || snapshot.status.mode === "live") &&
    manualEntryReason.trim().length > 0 &&
    isPositiveNumberInput(manualEntryQuantity) &&
    isPositiveNumberInput(manualEntryTickSize) &&
    isPositiveNumberInput(manualEntryReferencePrice) &&
    (manualEntryTickValueUsd.trim().length === 0 ||
      isPositiveNumberInput(manualEntryTickValueUsd));
  const canClosePosition =
    (snapshot?.history.projection.open_position_symbols.length ?? 0) > 0 &&
    closePositionReason.trim().length > 0 &&
    snapshot?.status.command_dispatch_ready === true;
  const canCancelWorkingOrders =
    openWorkingOrders.length > 0 &&
    cancelWorkingOrdersReason.trim().length > 0 &&
    snapshot?.status.command_dispatch_ready === true;
  const canLoadSelectedStrategy =
    strategyViewModel.selectedPath.length > 0 &&
    strategyViewModel.validation?.valid === true &&
    pendingAction === null;
  const canUploadSelectedStrategyFile =
    selectedStrategyUploadFile !== null && pendingAction === null;
  const canDisableNewEntries =
    snapshot != null &&
    pendingAction === null &&
    snapshot.status.operator_new_entries_enabled === true;
  const canEnableNewEntries =
    snapshot != null &&
    pendingAction === null &&
    snapshot.status.operator_new_entries_enabled === false;
  const canSaveSettings =
    snapshot != null && settingsDraft != null && settingsDirty && pendingAction === null;
  const reviewActionsDisabled = reviewButtonDisabled(pendingAction, snapshot);
  const reconnectCloseDisabled =
    reviewActionsDisabled || snapshot?.status.reconnect_review.required !== true;
  const shutdownLeaveDisabled =
    reviewActionsDisabled ||
    snapshot?.status.shutdown_review.blocked !== true ||
    snapshot.status.shutdown_review.all_positions_broker_protected !== true;
  const shutdownFlattenDisabled =
    reviewActionsDisabled || snapshot?.status.shutdown_review.blocked !== true;

  return (
    <main className="shell">
      <section className={`system-bar system-bar--${headlineTone}`}>
        <div className="system-bar__intro">
          <div className="system-bar__copy">
            <p className="eyebrow">TV Bot Operator Console</p>
            <h1>Local runtime command center</h1>
            <p className="system-bar__summary">
              The loaded contract chart is now the center of the workspace, with runtime posture,
              contract context, and operator actions arranged around it.
            </p>
          </div>
          <div className="system-bar__meta">
            <div className="system-bar__mode-lockup">
              <span className="system-bar__mode-label">Current mode</span>
              <strong>{snapshot ? formatMode(snapshot.status.mode) : "Waiting for runtime"}</strong>
              <span className="system-bar__mode-detail">{activeReviewSummary}</span>
            </div>
            <div className="system-bar__actions">
              <button
                className="refresh-button"
                type="button"
                onClick={() => {
                  void refreshSnapshot();
                }}
              >
                Refresh now
              </button>
              <p className="system-bar__timestamp">
                Last sync{" "}
                {snapshot
                  ? formatDateTime(snapshot.fetchedAt)
                  : formatDateTime(viewModel.lastAttemptedAt)}
              </p>
            </div>
          </div>
        </div>
        <div className="system-bar__signals" aria-label="Runtime posture">
          <SignalTile
            label="Arm state"
            value={snapshot ? formatMode(snapshot.status.arm_state) : "Waiting"}
            detail={
              snapshot
                ? snapshot.status.strategy_loaded
                  ? "Strategy is loaded and tracked by the host"
                  : "No strategy is currently loaded"
                : "Polling local runtime host"
            }
            tone={
              snapshot
                ? snapshot.status.arm_state === "armed"
                  ? "healthy"
                  : "neutral"
                : "info"
            }
          />
          <SignalTile
            label="Readiness"
            value={snapshot ? readinessState : "Waiting"}
            detail={
              snapshot
                ? `${readinessCounts.pass} pass | ${readinessCounts.warning} warning`
                : "Waiting for grouped checks"
            }
            tone={snapshot ? readinessTone(readinessCounts) : "info"}
          />
          <SignalTile
            label="Warmup"
            value={snapshot ? formatMode(snapshot.status.warmup_status) : "Waiting"}
            detail={
              snapshot?.status.market_data_status?.warmup_mode
                ? formatWarmupMode(snapshot.status.market_data_status.warmup_mode)
                : "Awaiting market-data state"
            }
            tone={snapshot ? warmupTone(snapshot.status.warmup_status) : "info"}
          />
          <SignalTile
            label="Dispatch"
            value={
              snapshot
                ? snapshot.status.command_dispatch_ready
                  ? "Ready"
                  : "Blocked"
                : "Waiting"
            }
            detail={
              snapshot
                ? snapshot.status.command_dispatch_ready
                  ? snapshot.status.current_account_name ?? "Runtime host is dispatch-ready"
                  : snapshot.status.command_dispatch_detail
                : "Waiting for dispatcher state"
            }
            tone={snapshot ? dispatchTone(snapshot.status) : "info"}
          />
          <SignalTile
            label="Safety review"
            value={
              snapshot
                ? snapshot.status.reconnect_review.required ||
                  snapshot.status.shutdown_review.blocked ||
                  snapshot.status.shutdown_review.awaiting_flatten
                  ? "Attention"
                  : "Clear"
                : "Waiting"
            }
            detail={
              snapshot
                ? activeReviewSummary
                : "Waiting for reconnect and shutdown review state"
            }
            tone={
              snapshot
                ? snapshot.status.reconnect_review.required ||
                  snapshot.status.shutdown_review.blocked ||
                  snapshot.status.shutdown_review.awaiting_flatten
                  ? "warning"
                  : "healthy"
                : "info"
            }
          />
        </div>
      </section>

      {viewModel.error ? (
        <section className="banner banner--warning" role="status">
          <strong>Local control-plane read failed.</strong>
          <span>{viewModel.error}</span>
        </section>
      ) : null}

      {commandFeedback ? (
        <section className={`banner banner--${commandFeedback.tone}`} role="status">
          <strong>Operator action result.</strong>
          <span>{commandFeedback.message}</span>
        </section>
      ) : null}

      {pendingAction ? (
        <section className="banner banner--info" role="status">
          <strong>Action in progress.</strong>
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
        <div className="workspace-shell">
          <div className="workspace-stage">
            <aside className="workspace-stage__rail workspace-stage__rail--context">
              <RuntimeSummaryPanel snapshot={snapshot} />
              <ReadinessPanel snapshot={snapshot} readinessCounts={readinessCounts} />
            </aside>

            <section className="workspace-stage__chart">
              <LiveChartPanel
                chartViewModel={chartViewModel}
                runtimeStatus={snapshot?.status ?? null}
                onSelectTimeframe={setSelectedTimeframe}
                onLoadOlderHistory={() => {
                  void loadOlderHistory();
                }}
                onRefreshChart={() => {
                  void refreshChart();
                }}
              />
            </section>

            <aside className="workspace-stage__rail workspace-stage__rail--actions">
              <ControlCenterPanel
                snapshot={snapshot}
                pendingAction={pendingAction}
                newEntriesReason={newEntriesReason}
                closePositionReason={closePositionReason}
                manualEntrySide={manualEntrySide}
                manualEntryQuantity={manualEntryQuantity}
                manualEntryTickSize={manualEntryTickSize}
                manualEntryReferencePrice={manualEntryReferencePrice}
                manualEntryTickValueUsd={manualEntryTickValueUsd}
                manualEntryReason={manualEntryReason}
                cancelWorkingOrdersReason={cancelWorkingOrdersReason}
                armButtonLabel={armButtonLabel}
                pauseButtonLabel={pauseButtonLabel}
                canDisableNewEntries={canDisableNewEntries}
                canEnableNewEntries={canEnableNewEntries}
                canManualEntry={canManualEntry}
                canClosePosition={canClosePosition}
                canCancelWorkingOrders={canCancelWorkingOrders}
                onSetMode={handleSetMode}
                onNewEntriesReasonChange={setNewEntriesReason}
                onSetNewEntriesEnabled={(enabled) => {
                  void updateNewEntriesEnabled(enabled);
                }}
                onStartWarmup={handleStartWarmup}
                onArmToggle={handleArmToggle}
                onPauseResume={handlePauseResume}
                onManualEntrySideChange={setManualEntrySide}
                onManualEntryQuantityChange={setManualEntryQuantity}
                onManualEntryTickSizeChange={setManualEntryTickSize}
                onManualEntryReferencePriceChange={setManualEntryReferencePrice}
                onManualEntryTickValueUsdChange={setManualEntryTickValueUsd}
                onManualEntryReasonChange={setManualEntryReason}
                onManualEntrySubmit={handleManualEntrySubmit}
                onClosePositionReasonChange={setClosePositionReason}
                onClosePositionSubmit={handleClosePositionSubmit}
                onCancelWorkingOrdersReasonChange={setCancelWorkingOrdersReason}
                onCancelWorkingOrdersSubmit={handleCancelWorkingOrdersSubmit}
              />

              <SafetyPanel
                snapshot={snapshot}
                reconnectReason={reconnectReason}
                shutdownReason={shutdownReason}
                reviewActionsDisabled={reviewActionsDisabled}
                reconnectCloseDisabled={reconnectCloseDisabled}
                shutdownLeaveDisabled={shutdownLeaveDisabled}
                shutdownFlattenDisabled={shutdownFlattenDisabled}
                onReconnectReasonChange={setReconnectReason}
                onShutdownReasonChange={setShutdownReason}
                onReconnectDecision={(decision) => {
                  void executeReconnectDecision(decision);
                }}
                onShutdownDecision={(decision) => {
                  void executeShutdownDecision(decision);
                }}
              />
            </aside>
          </div>

          <section className="workspace-dock">
            <div className="workspace-dock__header">
              <div>
                <p className="eyebrow">Detail Dock</p>
                <h2>Monitoring, audit, and configuration depth</h2>
                <p className="workspace-dock__summary">
                  Keep the chart in view while setup, trade detail, health, and audit surfaces
                  move through focused dock tabs beneath the workspace stage.
                </p>
              </div>
              <div className="workspace-dock__tabs" role="tablist" aria-label="Workspace dock">
                {workspaceDockSections.map(({ section, label }) => (
                  <button
                    key={section}
                    className={
                      activeDockSection === section
                        ? "workspace-dock__tab workspace-dock__tab--active"
                        : "workspace-dock__tab"
                    }
                    type="button"
                    role="tab"
                    id={`workspace-dock-tab-${section}`}
                    aria-selected={activeDockSection === section}
                    aria-controls={`workspace-dock-panel-${section}`}
                    onClick={() => {
                      setActiveDockSection(section);
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>

            <div
              className="workspace-dock__panel"
              id={`workspace-dock-panel-${activeDockSection}`}
              role="tabpanel"
              aria-labelledby={`workspace-dock-tab-${activeDockSection}`}
            >
              {activeDockSection === "setup" ? (
                <StrategySetupPanel
                  snapshot={snapshot}
                  pendingAction={pendingAction}
                  strategyViewModel={strategyViewModel}
                  selectedStrategyEntry={selectedStrategyEntry}
                  selectedStrategyUploadFile={selectedStrategyUploadFile}
                  strategyUploadInputRef={strategyUploadInputRef}
                  settingsDraft={settingsDraft}
                  settingsDirty={settingsDirty}
                  canLoadSelectedStrategy={canLoadSelectedStrategy}
                  canUploadSelectedStrategyFile={canUploadSelectedStrategyFile}
                  canSaveSettings={canSaveSettings}
                  onStrategyPathChange={handleStrategyPathChange}
                  onStrategyUploadFileChange={setSelectedStrategyUploadFile}
                  onUploadSelectedStrategyFile={() => {
                    void uploadSelectedStrategyFile();
                  }}
                  onRefreshStrategyLibrary={() => {
                    void refreshStrategyLibrary();
                  }}
                  onRefreshStrategyValidation={() => {
                    void refreshStrategyValidation(strategyViewModel.selectedPath);
                  }}
                  onLoadSelectedStrategy={handleLoadSelectedStrategy}
                  onSettingsStartupModeChange={(mode) => {
                    updateSettingsDraft((current) => ({ ...current, startupMode: mode }));
                  }}
                  onSettingsDefaultStrategyPathChange={(value) => {
                    updateSettingsDraft((current) => ({ ...current, defaultStrategyPath: value }));
                  }}
                  onSettingsAllowSqliteFallbackChange={(enabled) => {
                    updateSettingsDraft((current) => ({ ...current, allowSqliteFallback: enabled }));
                  }}
                  onSettingsPaperAccountNameChange={(value) => {
                    updateSettingsDraft((current) => ({ ...current, paperAccountName: value }));
                  }}
                  onSettingsLiveAccountNameChange={(value) => {
                    updateSettingsDraft((current) => ({ ...current, liveAccountName: value }));
                  }}
                  onSaveRuntimeSettings={() => {
                    void saveRuntimeSettings();
                  }}
                  onResetSettings={handleSettingsReset}
                />
              ) : null}

              {activeDockSection === "history" ? (
                <HistoryPanel
                  snapshot={snapshot}
                  openWorkingOrders={openWorkingOrders}
                  recentFills={recentFills}
                  recentTrades={recentTrades}
                  tradePerformance={tradePerformance}
                  pnlChart={pnlChart}
                  pnlChartPathData={pnlChartPathData}
                  perTradePnl={perTradePnl}
                  projectedPnlSnapshot={projectedPnlSnapshot}
                />
              ) : null}

              {activeDockSection === "health" ? (
                <HealthPanel snapshot={snapshot} feedStatuses={feedStatuses} />
              ) : null}

              {activeDockSection === "latency" ? (
                <LatencyPanel
                  snapshot={snapshot}
                  latencyBreakdown={latencyBreakdown}
                  slowestLatencyStage={slowestLatencyStage}
                />
              ) : null}

              {activeDockSection === "journal" ? (
                <JournalPanel
                  snapshot={snapshot}
                  journalSummary={journalSummary}
                  journalRecords={journalRecords}
                />
              ) : null}

              {activeDockSection === "events" ? (
                <EventsPanel
                  eventFeed={eventFeed}
                  eventHeadlineSummary={eventHeadlineSummary}
                />
              ) : null}
            </div>
          </section>
        </div>
      ) : null}
    </main>
  );
}

export default App;
