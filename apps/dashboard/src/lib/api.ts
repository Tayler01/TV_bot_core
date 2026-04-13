import type {
  ControlApiEvent,
  RuntimeHistorySnapshot,
  RuntimeHostHealthResponse,
  RuntimeJournalSnapshot,
  RuntimeLifecycleCommand,
  RuntimeLifecycleRequest,
  RuntimeLifecycleResponse,
  RuntimeReadinessSnapshot,
  RuntimeStrategyLibraryResponse,
  RuntimeStrategyUploadRequest,
  RuntimeStrategyValidationRequest,
  RuntimeStrategyValidationResponse,
  RuntimeStatusSnapshot,
} from "../types/controlApi";

export interface DashboardSnapshot {
  status: RuntimeStatusSnapshot;
  readiness: RuntimeReadinessSnapshot;
  history: RuntimeHistorySnapshot;
  journal: RuntimeJournalSnapshot;
  health: RuntimeHostHealthResponse;
  fetchedAt: string;
}

export class ControlApiError extends Error {
  readonly endpoint: string;
  readonly statusCode: number;

  constructor(endpoint: string, statusCode: number, message: string) {
    super(message);
    this.name = "ControlApiError";
    this.endpoint = endpoint;
    this.statusCode = statusCode;
  }
}

export interface LifecycleCommandResult {
  httpStatus: number;
  response: RuntimeLifecycleResponse;
}

const CONTROL_API_BASE_URL = (
  import.meta.env.VITE_CONTROL_API_BASE_URL ?? ""
).replace(/\/$/, "");
const CONTROL_API_EVENTS_URL = (
  import.meta.env.VITE_CONTROL_API_EVENTS_URL ?? ""
).replace(/\/$/, "");

async function readBody(response: Response): Promise<string> {
  const contentType = response.headers.get("content-type") ?? "";

  if (contentType.includes("application/json")) {
    const payload = (await response.json()) as { message?: string; body?: { message?: string } };
    return payload.body?.message ?? payload.message ?? `${response.status} ${response.statusText}`;
  }

  const body = await response.text();
  return body || `${response.status} ${response.statusText}`;
}

async function fetchJson<T>(endpoint: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(`${CONTROL_API_BASE_URL}${endpoint}`, {
    headers: {
      Accept: "application/json",
    },
    signal,
  });

  if (!response.ok) {
    throw new ControlApiError(endpoint, response.status, await readBody(response));
  }

  return (await response.json()) as T;
}

async function parseLifecycleResponse(response: Response): Promise<RuntimeLifecycleResponse> {
  const contentType = response.headers.get("content-type") ?? "";

  if (!contentType.includes("application/json")) {
    throw new ControlApiError(
      "/runtime/commands",
      response.status,
      "Runtime command response was not JSON.",
    );
  }

  return (await response.json()) as RuntimeLifecycleResponse;
}

export async function loadDashboardSnapshot(
  signal?: AbortSignal,
): Promise<DashboardSnapshot> {
  const [status, readiness, history, journal, health] = await Promise.all([
    fetchJson<RuntimeStatusSnapshot>("/status", signal),
    fetchJson<RuntimeReadinessSnapshot>("/readiness", signal),
    fetchJson<RuntimeHistorySnapshot>("/history", signal),
    fetchJson<RuntimeJournalSnapshot>("/journal", signal),
    fetchJson<RuntimeHostHealthResponse>("/health", signal),
  ]);

  return {
    status,
    readiness,
    history,
    journal,
    health,
    fetchedAt: new Date().toISOString(),
  };
}

export async function sendLifecycleCommand(
  command: RuntimeLifecycleCommand,
  signal?: AbortSignal,
): Promise<LifecycleCommandResult> {
  const request: RuntimeLifecycleRequest = {
    source: "dashboard",
    command,
  };

  const response = await fetch(`${CONTROL_API_BASE_URL}/runtime/commands`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  });

  return {
    httpStatus: response.status,
    response: await parseLifecycleResponse(response),
  };
}

export async function loadStrategyLibrary(
  signal?: AbortSignal,
): Promise<RuntimeStrategyLibraryResponse> {
  return fetchJson<RuntimeStrategyLibraryResponse>("/strategies", signal);
}

export async function validateStrategyPath(
  path: string,
  signal?: AbortSignal,
): Promise<RuntimeStrategyValidationResponse> {
  const request: RuntimeStrategyValidationRequest = {
    source: "dashboard",
    path,
  };

  const response = await fetch(`${CONTROL_API_BASE_URL}/strategies/validate`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  });

  if (!response.ok) {
    throw new ControlApiError("/strategies/validate", response.status, await readBody(response));
  }

  return (await response.json()) as RuntimeStrategyValidationResponse;
}

export async function uploadStrategyMarkdown(
  filename: string,
  markdown: string,
  signal?: AbortSignal,
): Promise<RuntimeStrategyValidationResponse> {
  const request: RuntimeStrategyUploadRequest = {
    source: "dashboard",
    filename,
    markdown,
  };

  const response = await fetch(`${CONTROL_API_BASE_URL}/strategies/upload`, {
    method: "POST",
    headers: {
      Accept: "application/json",
      "Content-Type": "application/json",
    },
    body: JSON.stringify(request),
    signal,
  });

  if (!response.ok) {
    throw new ControlApiError("/strategies/upload", response.status, await readBody(response));
  }

  return (await response.json()) as RuntimeStrategyValidationResponse;
}

function defaultEventsUrl(): string {
  if (typeof window === "undefined") {
    return "ws://127.0.0.1:8081/events";
  }

  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/events`;
}

export function controlApiEventsUrl(): string {
  if (CONTROL_API_EVENTS_URL) {
    return `${CONTROL_API_EVENTS_URL}/events`;
  }

  if (CONTROL_API_BASE_URL) {
    const url = new URL(`${CONTROL_API_BASE_URL}/events`, window.location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return url.toString();
  }

  return defaultEventsUrl();
}

export function parseControlApiEvent(payload: string): ControlApiEvent {
  return JSON.parse(payload) as ControlApiEvent;
}
