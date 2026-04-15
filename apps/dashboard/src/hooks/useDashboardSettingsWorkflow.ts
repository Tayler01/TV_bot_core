import {
  useEffect,
  useEffectEvent,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";

import { loadDashboardSnapshot, updateRuntimeSettings, type DashboardSnapshot } from "../lib/api";
import {
  runtimeSettingsRequestFromDraft,
  settingsDraftFromSnapshot,
} from "../lib/dashboardProjection";
import type { CommandFeedback, RuntimeSettingsDraft } from "../dashboardModels";

interface UseDashboardSettingsWorkflowOptions {
  snapshot: DashboardSnapshot | null;
  setPendingAction: Dispatch<SetStateAction<string | null>>;
  setCommandFeedback: Dispatch<SetStateAction<CommandFeedback | null>>;
  updateSnapshot: (
    updater: (snapshot: DashboardSnapshot | null) => DashboardSnapshot | null,
  ) => void;
}

export function useDashboardSettingsWorkflow({
  snapshot,
  setPendingAction,
  setCommandFeedback,
  updateSnapshot,
}: UseDashboardSettingsWorkflowOptions) {
  const settingsDraftRef = useRef<RuntimeSettingsDraft | null>(null);
  const [settingsDraft, setSettingsDraft] = useState<RuntimeSettingsDraft | null>(null);
  const [settingsDirty, setSettingsDirty] = useState(false);

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

  const resetSettings = useEffectEvent(() => {
    if (!snapshot) {
      return;
    }

    const nextDraft = settingsDraftFromSnapshot(snapshot.settings);
    settingsDraftRef.current = nextDraft;
    setSettingsDraft(nextDraft);
    setSettingsDirty(false);
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

      updateSnapshot((currentSnapshot) =>
        refreshedSnapshot ??
        (currentSnapshot
          ? {
              ...currentSnapshot,
              settings: result.settings,
              fetchedAt: new Date().toISOString(),
            }
          : null),
      );

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

  useEffect(() => {
    if (!snapshot || settingsDirty) {
      return;
    }

    const nextDraft = settingsDraftFromSnapshot(snapshot.settings);
    settingsDraftRef.current = nextDraft;
    setSettingsDraft(nextDraft);
  }, [settingsDirty, snapshot]);

  return {
    settingsDraft,
    settingsDirty,
    updateSettingsDraft,
    resetSettings,
    saveRuntimeSettings,
  };
}
