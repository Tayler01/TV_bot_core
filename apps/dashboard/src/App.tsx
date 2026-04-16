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
} from "./components/dashboardControlPanels";
import { LiveChartPanel } from "./components/dashboardLiveChart";
import { SignalTile } from "./components/dashboardPrimitives";
import type { LatencyStageViewModel } from "./dashboardModels";
import { useDashboardController } from "./hooks/useDashboardController";
import { useDashboardChart } from "./hooks/useDashboardChart";

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
      <div className={`hero hero--${headlineTone}`}>
        <div className="hero__content">
          <div className="hero__copy">
            <p className="eyebrow">TV Bot Operator Console</p>
            <h1>Local runtime command center</h1>
            <p className="hero__summary">
              Operate the runtime, watch the live safety posture, and resolve review-required
              states from the local control plane without losing the backend as the source of
              truth.
            </p>
          </div>
          <div className="hero__meta">
            <div className="hero__mode-lockup">
              <span className="hero__mode-label">Current mode</span>
              <strong>{snapshot ? formatMode(snapshot.status.mode) : "Waiting for runtime"}</strong>
              <span className="hero__mode-detail">{activeReviewSummary}</span>
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
                {snapshot
                  ? formatDateTime(snapshot.fetchedAt)
                  : formatDateTime(viewModel.lastAttemptedAt)}
              </p>
            </div>
          </div>
        </div>
        <div className="hero__rail" aria-label="Runtime posture">
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
      </div>

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
        <div className="dashboard-grid">
          <ControlCenterPanel
            snapshot={snapshot}
            pendingAction={pendingAction}
            strategyViewModel={strategyViewModel}
            selectedStrategyEntry={selectedStrategyEntry}
            selectedStrategyUploadFile={selectedStrategyUploadFile}
            strategyUploadInputRef={strategyUploadInputRef}
            settingsDraft={settingsDraft}
            settingsDirty={settingsDirty}
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
            canLoadSelectedStrategy={canLoadSelectedStrategy}
            canUploadSelectedStrategyFile={canUploadSelectedStrategyFile}
            canDisableNewEntries={canDisableNewEntries}
            canEnableNewEntries={canEnableNewEntries}
            canSaveSettings={canSaveSettings}
            canManualEntry={canManualEntry}
            canClosePosition={canClosePosition}
            canCancelWorkingOrders={canCancelWorkingOrders}
            onSetMode={handleSetMode}
            onNewEntriesReasonChange={setNewEntriesReason}
            onSetNewEntriesEnabled={(enabled) => {
              void updateNewEntriesEnabled(enabled);
            }}
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

          <RuntimeSummaryPanel snapshot={snapshot} />

          <ReadinessPanel snapshot={snapshot} readinessCounts={readinessCounts} />

          <HealthPanel snapshot={snapshot} feedStatuses={feedStatuses} />

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

          <LatencyPanel
            snapshot={snapshot}
            latencyBreakdown={latencyBreakdown}
            slowestLatencyStage={slowestLatencyStage}
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

          <JournalPanel
            snapshot={snapshot}
            journalSummary={journalSummary}
            journalRecords={journalRecords}
          />

          <EventsPanel
            eventFeed={eventFeed}
            eventHeadlineSummary={eventHeadlineSummary}
          />
        </div>
      ) : null}
    </main>
  );
}

export default App;
