import {
  useEffect,
  useEffectEvent,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";

import {
  loadStrategyLibrary,
  uploadStrategyMarkdown,
  validateStrategyPath,
} from "../lib/api";
import { selectStrategyPath } from "../lib/dashboardProjection";
import type { CommandFeedback, StrategySummaryViewModel } from "../dashboardModels";

const INITIAL_STRATEGY_VIEW_MODEL: StrategySummaryViewModel = {
  library: null,
  validation: null,
  libraryError: null,
  validationError: null,
  libraryState: "idle",
  validationState: "idle",
  selectedPath: "",
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

interface UseDashboardStrategyWorkflowOptions {
  setPendingAction: Dispatch<SetStateAction<string | null>>;
  setCommandFeedback: Dispatch<SetStateAction<CommandFeedback | null>>;
  strategyUploadInputRef: RefObject<HTMLInputElement | null>;
}

export function useDashboardStrategyWorkflow({
  setPendingAction,
  setCommandFeedback,
  strategyUploadInputRef,
}: UseDashboardStrategyWorkflowOptions) {
  const [strategyViewModel, setStrategyViewModel] = useState<StrategySummaryViewModel>(
    INITIAL_STRATEGY_VIEW_MODEL,
  );
  const [selectedStrategyUploadFile, setSelectedStrategyUploadFile] = useState<File | null>(null);

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
    void refreshStrategyLibrary(controller.signal);

    return () => {
      controller.abort();
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

  return {
    strategyViewModel,
    setStrategyViewModel,
    selectedStrategyUploadFile,
    setSelectedStrategyUploadFile,
    refreshStrategyLibrary,
    refreshStrategyValidation,
    uploadSelectedStrategyFile,
  };
}
