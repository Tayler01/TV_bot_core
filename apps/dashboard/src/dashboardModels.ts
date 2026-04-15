import type { DashboardSnapshot } from "./lib/api";
import type {
  RuntimeMode,
  RuntimeStrategyLibraryResponse,
  RuntimeStrategyValidationResponse,
  TradeSummaryRecord,
} from "./types/controlApi";

export type LoadState = "idle" | "loading" | "ready" | "error";
export type BannerTone = "healthy" | "warning" | "danger" | "info";
export type EventConnectionState = "connecting" | "open" | "closed" | "error" | "unsupported";
export type SignalTone = BannerTone | "paper" | "live" | "neutral";

export interface ViewModel {
  snapshot: DashboardSnapshot | null;
  loadState: LoadState;
  error: string | null;
  lastAttemptedAt: string | null;
}

export interface CommandFeedback {
  tone: BannerTone;
  message: string;
}

export interface CommandOptions {
  confirmMessage?: string;
  pendingLabel: string;
}

export interface StrategySummaryViewModel {
  library: RuntimeStrategyLibraryResponse | null;
  validation: RuntimeStrategyValidationResponse | null;
  libraryError: string | null;
  validationError: string | null;
  libraryState: LoadState;
  validationState: LoadState;
  selectedPath: string;
}

export interface EventFeedItem {
  id: string;
  headline: string;
  detail: string;
  tone: BannerTone;
  occurredAt: string;
}

export interface EventFeedViewModel {
  connectionState: EventConnectionState;
  recentEvents: EventFeedItem[];
  lastEventAt: string | null;
  error: string | null;
}

export interface RuntimeSettingsDraft {
  startupMode: RuntimeMode;
  defaultStrategyPath: string;
  allowSqliteFallback: boolean;
  paperAccountName: string;
  liveAccountName: string;
}

export interface TradePerformanceViewModel {
  closedCount: number;
  openCount: number;
  winRate: number | null;
  averageNet: number | null;
  averageHoldMinutes: number | null;
  largestWin: number | null;
  largestLoss: number | null;
  floatingNet: number | null;
}

export interface PnlChartPoint {
  id: string;
  label: string;
  note: string;
  value: number;
  xPercent: number;
  yPercent: number;
  tone: BannerTone;
}

export interface PnlChartViewModel {
  points: PnlChartPoint[];
  zeroPercent: number | null;
}

export interface PerTradePnlViewModel {
  tradeId: string;
  symbol: string;
  side: TradeSummaryRecord["side"];
  quantity: number;
  status: TradeSummaryRecord["status"];
  netPnl: number | null;
  grossPnl: number | null;
  fees: number | null;
  commissions: number | null;
  slippage: number | null;
  holdMinutes: number | null;
  openedAt: string;
  closedAt: string | null;
  tone: BannerTone;
}

export interface JournalCategoryCount {
  category: string;
  count: number;
}

export interface JournalSummaryViewModel {
  infoCount: number;
  warningCount: number;
  errorCount: number;
  dashboardCount: number;
  systemCount: number;
  cliCount: number;
  categories: JournalCategoryCount[];
}

export interface HeadlineSummary {
  headline: string;
  count: number;
  tone: BannerTone;
}

export interface LatencyStageViewModel {
  key: string;
  label: string;
  value: number | null;
  barPercent: number;
}
