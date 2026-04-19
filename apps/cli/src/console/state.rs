use std::{
    collections::{hash_map::DefaultHasher, VecDeque},
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use ratatui::{
    style::Color,
    text::{Line, Span},
};
use reqwest::Client;
use tv_bot_control_api::{
    ControlApiCommandStatus, ControlApiEvent, RuntimeChartConfigResponse, RuntimeChartSnapshot,
    RuntimeChartStreamEvent, RuntimeHistorySnapshot, RuntimeJournalSnapshot,
    RuntimeLifecycleCommand, RuntimeLifecycleResponse, RuntimeReadinessSnapshot,
    RuntimeStatusSnapshot,
};
use tv_bot_core_types::{ActionSource, RuntimeMode, Timeframe};

use crate::commands::{
    arm_state_label, chart_snapshot_limit, fetch_chart_config, fetch_chart_snapshot, fetch_history,
    fetch_journal, fetch_readiness, fetch_status, format_optional_bytes, format_optional_percent,
    runtime_mode_label, timeframe_label, timeframe_query_value, warmup_status_label,
};

use super::app::{ConsoleMessage, StreamStatus};

const MAX_ACTIVITY_ITEMS: usize = 14;
const DEFAULT_CHART_VIEWPORT_WIDTH: usize = 1_440;

pub struct ConsoleState {
    status: Option<RuntimeStatusSnapshot>,
    readiness: Option<RuntimeReadinessSnapshot>,
    history: Option<RuntimeHistorySnapshot>,
    journal: Option<RuntimeJournalSnapshot>,
    chart_config: Option<RuntimeChartConfigResponse>,
    chart_snapshot: Option<RuntimeChartSnapshot>,
    error: Option<String>,
    refresh_interval: Duration,
    next_refresh_at: Instant,
    last_refresh_at: Option<Instant>,
    activity_feed: VecDeque<ActivityEntry>,
    activity_filter: ActivityFilter,
    event_stream_status: StreamStatusLabel,
    chart_stream_status: StreamStatusLabel,
    event_stream_url: Option<String>,
    chart_stream_url_base: Option<String>,
    event_stream_fingerprint: Option<u64>,
    chart_stream_fingerprint: Option<u64>,
    selected_timeframe: Option<Timeframe>,
    last_fill_id: Option<String>,
    last_order_id: Option<String>,
    show_help_overlay: bool,
    pending_confirmation: Option<PendingConfirmation>,
}

impl ConsoleState {
    pub fn new(refresh_interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            status: None,
            readiness: None,
            history: None,
            journal: None,
            chart_config: None,
            chart_snapshot: None,
            error: None,
            refresh_interval,
            next_refresh_at: now,
            last_refresh_at: None,
            activity_feed: VecDeque::new(),
            activity_filter: ActivityFilter::All,
            event_stream_status: StreamStatusLabel::Idle,
            chart_stream_status: StreamStatusLabel::Idle,
            event_stream_url: None,
            chart_stream_url_base: None,
            event_stream_fingerprint: None,
            chart_stream_fingerprint: None,
            selected_timeframe: None,
            last_fill_id: None,
            last_order_id: None,
            show_help_overlay: false,
            pending_confirmation: None,
        }
    }

    pub async fn refresh(&mut self, client: &Client, base_url: &str) {
        match refresh_snapshot(client, base_url, self.selected_timeframe).await {
            Ok(refresh) => {
                self.status = Some(refresh.status);
                self.readiness = Some(refresh.readiness);
                self.history = Some(refresh.history);
                self.journal = Some(refresh.journal);
                self.chart_config = Some(refresh.chart_config);
                self.chart_snapshot = refresh.chart_snapshot;
                self.selected_timeframe = refresh.selected_timeframe;
                self.event_stream_url = refresh.event_stream_url;
                self.chart_stream_url_base = refresh.chart_stream_url_base;
                self.event_stream_fingerprint = self
                    .event_stream_url
                    .as_ref()
                    .map(|value| stream_fingerprint(value));
                self.chart_stream_fingerprint = self
                    .chart_stream_url()
                    .as_ref()
                    .map(|value| stream_fingerprint(value));
                self.error = None;
                self.last_refresh_at = Some(Instant::now());
                self.push_baseline_activity();
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.push_activity(
                    ActivityCategory::Warning,
                    ActivityLevel::Error,
                    format!("runtime host refresh failed: {error}"),
                );
            }
        }

        self.next_refresh_at = Instant::now() + self.refresh_interval;
    }

    pub async fn refresh_chart(&mut self, client: &Client, base_url: &str) {
        let Some(chart_stream_url_base) = self.chart_stream_url_base.clone() else {
            return;
        };

        match fetch_chart_config(client, base_url).await {
            Ok(chart_config) => {
                let selected_timeframe = choose_timeframe(&chart_config, self.selected_timeframe);
                self.chart_config = Some(chart_config.clone());
                self.selected_timeframe = selected_timeframe;
                self.chart_stream_url_base = Some(chart_stream_url_base);
                self.chart_stream_fingerprint = self
                    .chart_stream_url()
                    .as_ref()
                    .map(|value| stream_fingerprint(value));

                self.chart_snapshot = if chart_config.available {
                    match selected_timeframe {
                        Some(timeframe) => {
                            match fetch_chart_snapshot(
                                client,
                                base_url,
                                timeframe,
                                chart_snapshot_limit(timeframe, DEFAULT_CHART_VIEWPORT_WIDTH),
                            )
                            .await
                            {
                                Ok(snapshot) => Some(snapshot),
                                Err(error) => {
                                    self.push_activity(
                                        ActivityCategory::Warning,
                                        ActivityLevel::Error,
                                        format!("chart snapshot refresh failed: {error}"),
                                    );
                                    None
                                }
                            }
                        }
                        None => None,
                    }
                } else {
                    None
                };
            }
            Err(error) => {
                self.push_activity(
                    ActivityCategory::Warning,
                    ActivityLevel::Error,
                    format!("chart config refresh failed: {error}"),
                );
            }
        }
    }

    pub fn refresh_due(&self) -> bool {
        Instant::now() >= self.next_refresh_at
    }

    pub fn mode_label(&self) -> String {
        self.status
            .as_ref()
            .map(|status| runtime_mode_label(&status.mode).to_owned())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    pub fn mode_color(&self) -> Color {
        match self.status.as_ref().map(|status| &status.mode) {
            Some(tv_bot_core_types::RuntimeMode::Live) => Color::Red,
            Some(tv_bot_core_types::RuntimeMode::Paper) => Color::Green,
            Some(tv_bot_core_types::RuntimeMode::Observation) => Color::Cyan,
            Some(tv_bot_core_types::RuntimeMode::Paused) => Color::Yellow,
            None => Color::DarkGray,
        }
    }

    pub fn left_summary_lines(&self) -> Vec<Line<'static>> {
        let Some(status) = &self.status else {
            return vec![Line::from("Connecting to the local runtime host...")];
        };

        vec![
            labeled("Arm", arm_state_label(&status.arm_state)),
            labeled("Warmup", warmup_status_label(&status.warmup_status)),
            labeled(
                "Strategy",
                status
                    .current_strategy
                    .as_ref()
                    .map(|strategy| strategy.name.as_str())
                    .unwrap_or("none loaded"),
            ),
            labeled(
                "Contract",
                status
                    .instrument_mapping
                    .as_ref()
                    .map(|mapping| mapping.tradovate_symbol.as_str())
                    .unwrap_or("unresolved"),
            ),
            labeled(
                "Account",
                status
                    .current_account_name
                    .as_deref()
                    .unwrap_or("none selected"),
            ),
            labeled("Refresh", self.last_refresh_summary().as_str()),
        ]
    }

    pub fn right_summary_lines(&self) -> Vec<Line<'static>> {
        let Some(status) = &self.status else {
            return vec![Line::from("Waiting for backend summary...")];
        };

        let broker = status
            .broker_status
            .as_ref()
            .map(|snapshot| {
                format!(
                    "{:?} / {:?} ({:?})",
                    snapshot.connection_state, snapshot.sync_state, snapshot.health
                )
            })
            .unwrap_or_else(|| "unavailable".to_owned());
        let market_data = status
            .market_data_status
            .as_ref()
            .map(|snapshot| {
                format!(
                    "{:?} / {:?} / ready={}",
                    snapshot.session.market_data.connection_state,
                    snapshot.session.market_data.health,
                    snapshot.trade_ready
                )
            })
            .unwrap_or_else(|| {
                status
                    .market_data_detail
                    .clone()
                    .unwrap_or_else(|| "unavailable".to_owned())
            });
        let storage = format!(
            "{} / durable={} / fallback={}",
            status.storage_status.active_backend,
            status.storage_status.durable,
            status.storage_status.fallback_activated
        );
        let dispatch = if status.command_dispatch_ready {
            "ready".to_owned()
        } else {
            format!("unavailable ({})", status.command_dispatch_detail)
        };
        let reconnect = if status.reconnect_review.required {
            status
                .reconnect_review
                .reason
                .clone()
                .unwrap_or_else(|| "review required".to_owned())
        } else {
            "clear".to_owned()
        };
        let shutdown = if status.shutdown_review.pending_signal || status.shutdown_review.blocked {
            status
                .shutdown_review
                .reason
                .clone()
                .unwrap_or_else(|| "review pending".to_owned())
        } else {
            "clear".to_owned()
        };

        vec![
            labeled("Broker", broker.as_str()),
            labeled("Market", market_data.as_str()),
            labeled("Dispatch", dispatch.as_str()),
            labeled("Storage", storage.as_str()),
            labeled("Reconnect", reconnect.as_str()),
            labeled("Shutdown", shutdown.as_str()),
        ]
    }

    pub fn chart_title(&self) -> String {
        match self.selected_timeframe {
            Some(timeframe) => format!("Chart Stage [{}]", timeframe_label(timeframe)),
            None => "Chart Stage".to_owned(),
        }
    }

    pub fn chart_summary_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        if let Some(chart_config) = &self.chart_config {
            lines.push(labeled(
                "Chart",
                if chart_config.available {
                    "available"
                } else {
                    chart_config.detail.as_str()
                },
            ));
            lines.push(labeled(
                "Instrument",
                chart_config
                    .instrument
                    .as_ref()
                    .map(|instrument| instrument.summary.as_str())
                    .unwrap_or("waiting for loaded strategy contract"),
            ));
            let supported = chart_config
                .supported_timeframes
                .iter()
                .map(|timeframe| timeframe_label(*timeframe))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(labeled(
                "Frames",
                if supported.is_empty() {
                    "none"
                } else {
                    supported.as_str()
                },
            ));
        }

        if let Some(chart_snapshot) = &self.chart_snapshot {
            let latest_price = chart_snapshot
                .latest_price
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".to_owned());
            let bars_line = format!(
                "{} bars | latest={} | fills={} | orders={}",
                chart_snapshot.bars.len(),
                latest_price,
                chart_snapshot.recent_fills.len(),
                chart_snapshot.working_orders.len()
            );
            lines.push(labeled("Bars", bars_line.as_str()));

            if let Some(position) = &chart_snapshot.active_position {
                let stance = if position.quantity > 0 {
                    "long"
                } else {
                    "short"
                };
                let average_price = position
                    .average_price
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_owned());
                let position_line =
                    format!("{stance} {} @ {average_price}", position.quantity.abs(),);
                lines.push(labeled("Position", position_line.as_str()));
            }

            if let Some(order) = chart_snapshot.working_orders.first() {
                let quantity = order
                    .quantity
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_owned());
                let mut descriptors = Vec::new();
                if let Some(limit_price) = order.limit_price {
                    descriptors.push(format!("LMT {limit_price}"));
                }
                if let Some(stop_price) = order.stop_price {
                    descriptors.push(format!("STP {stop_price}"));
                }
                let order_line = if descriptors.is_empty() {
                    format!("{:?} x{quantity} {:?}", order.side, order.status)
                } else {
                    format!(
                        "{:?} x{quantity} {:?} {}",
                        order.side,
                        order.status,
                        descriptors.join(" / ")
                    )
                };
                lines.push(labeled("Order", order_line.as_str()));
            }

            let mut guides = Vec::new();
            if chart_snapshot.latest_price.is_some() {
                guides.push("live");
            }
            if chart_snapshot.active_position.is_some() {
                guides.push("position");
            }
            if !chart_snapshot.recent_fills.is_empty() {
                guides.push("fills");
            }
            if !guides.is_empty() {
                lines.push(labeled("Guides", guides.join(" / ").as_str()));
            }
        }

        if let Some(system_health) = self
            .status
            .as_ref()
            .and_then(|status| status.system_health.as_ref())
        {
            let host_line = format!(
                "cpu {} | mem {}",
                format_optional_percent(system_health.cpu_percent),
                format_optional_bytes(system_health.memory_bytes)
            );
            lines.push(labeled("Host", host_line.as_str()));
        }

        lines.push(labeled("Event Stream", self.event_stream_status.as_str()));
        lines.push(labeled("Chart Stream", self.chart_stream_status.as_str()));

        if lines.is_empty() {
            lines.push(Line::from(
                "Waiting for runtime status before chart bootstrap.",
            ));
        }

        lines
    }

    pub fn chart_points(&self) -> Vec<(f64, f64)> {
        let Some(snapshot) = &self.chart_snapshot else {
            return vec![];
        };

        snapshot
            .bars
            .iter()
            .enumerate()
            .map(|(index, bar)| (index as f64, decimal_to_f64(&bar.close)))
            .collect()
    }

    pub fn chart_current_price_line(&self) -> Vec<(f64, f64)> {
        let Some(snapshot) = &self.chart_snapshot else {
            return vec![];
        };
        let Some(price) = snapshot.latest_price.as_ref().map(decimal_to_f64) else {
            return vec![];
        };

        horizontal_line(self.chart_x_bounds(), price)
    }

    pub fn chart_active_position_line(&self) -> Vec<(f64, f64)> {
        let Some(snapshot) = &self.chart_snapshot else {
            return vec![];
        };
        let Some(price) = snapshot
            .active_position
            .as_ref()
            .and_then(|position| position.average_price.as_ref())
            .map(decimal_to_f64)
        else {
            return vec![];
        };

        horizontal_line(self.chart_x_bounds(), price)
    }

    pub fn chart_fill_marker_points(&self) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
        let Some(snapshot) = &self.chart_snapshot else {
            return (vec![], vec![]);
        };

        let mut buys = Vec::new();
        let mut sells = Vec::new();

        for fill in &snapshot.recent_fills {
            let point = nearest_fill_marker_point(snapshot, fill);
            match fill.side {
                tv_bot_core_types::TradeSide::Buy => buys.push(point),
                tv_bot_core_types::TradeSide::Sell => sells.push(point),
            }
        }

        (buys, sells)
    }

    pub fn chart_x_bounds(&self) -> [f64; 2] {
        let points = self.chart_points();
        if points.len() < 2 {
            return [0.0, 1.0];
        }

        [0.0, (points.len() - 1) as f64]
    }

    pub fn chart_y_bounds(&self) -> [f64; 2] {
        let Some(snapshot) = &self.chart_snapshot else {
            return [0.0, 1.0];
        };

        let lows: Vec<f64> = snapshot
            .bars
            .iter()
            .map(|bar| decimal_to_f64(&bar.low))
            .collect();
        let highs: Vec<f64> = snapshot
            .bars
            .iter()
            .map(|bar| decimal_to_f64(&bar.high))
            .collect();
        if lows.is_empty() || highs.is_empty() {
            return [0.0, 1.0];
        }

        let low = lows.into_iter().fold(f64::INFINITY, f64::min);
        let high = highs.into_iter().fold(f64::NEG_INFINITY, f64::max);
        let spread = (high - low).max(0.25);
        let padding = spread * 0.12;

        [low - padding, high + padding]
    }

    pub fn chart_x_labels(&self) -> Vec<String> {
        let Some(snapshot) = &self.chart_snapshot else {
            return vec!["waiting".to_owned(), "live".to_owned()];
        };

        match (snapshot.bars.first(), snapshot.bars.last()) {
            (Some(first), Some(last)) if snapshot.bars.len() > 1 => vec![
                first.closed_at.format("%H:%M").to_string(),
                last.closed_at.format("%H:%M").to_string(),
            ],
            (Some(last), _) => vec![
                timeframe_label(snapshot.timeframe).to_owned(),
                last.closed_at.format("%H:%M").to_string(),
            ],
            _ => vec!["waiting".to_owned(), "live".to_owned()],
        }
    }

    pub fn chart_y_labels(&self) -> Vec<String> {
        let bounds = self.chart_y_bounds();
        vec![format!("{:.2}", bounds[0]), format!("{:.2}", bounds[1])]
    }

    pub fn activity_items(&self) -> Vec<ActivityEntryView> {
        let filtered: Vec<ActivityEntryView> = self
            .activity_feed
            .iter()
            .filter(|entry| self.activity_filter.matches(entry.category))
            .map(ActivityEntryView::from)
            .collect();

        if filtered.is_empty() {
            return vec![ActivityEntryView {
                label: "idle".to_owned(),
                message: "waiting for runtime activity from the local host".to_owned(),
                level: ActivityLevel::Info,
            }];
        }

        filtered
    }

    pub fn activity_title(&self) -> String {
        format!("Recent Activity [{}]", self.activity_filter.label())
    }

    pub fn cycle_activity_filter(&mut self) {
        self.activity_filter = self.activity_filter.next();
        self.push_activity(
            ActivityCategory::System,
            ActivityLevel::Info,
            format!("activity filter: {}", self.activity_filter.label()),
        );
    }

    pub fn footer_status(&self) -> String {
        if let Some(pending) = &self.pending_confirmation {
            return format!("confirm: {} [y/n]", pending.prompt);
        }

        if let Some(error) = &self.error {
            return format!("last refresh failed: {error}");
        }

        format!(
            "auto refresh every {}s | events {} | chart {}",
            self.refresh_interval.as_secs(),
            self.event_stream_status.as_str(),
            self.chart_stream_status.as_str()
        )
    }

    pub fn apply_message(&mut self, message: ConsoleMessage) {
        match message {
            ConsoleMessage::ControlEvent(event) => self.apply_control_event(event),
            ConsoleMessage::ChartEvent(event) => self.apply_chart_event(event),
            ConsoleMessage::EventStreamStatus(status) => {
                self.event_stream_status = StreamStatusLabel::from(status.clone());
                match status {
                    StreamStatus::Open => self.push_activity(
                        ActivityCategory::System,
                        ActivityLevel::Success,
                        "event stream connected".to_owned(),
                    ),
                    StreamStatus::Closed => self.push_activity(
                        ActivityCategory::Warning,
                        ActivityLevel::Warning,
                        "event stream closed".to_owned(),
                    ),
                    StreamStatus::Error(detail) => {
                        self.push_activity(ActivityCategory::Warning, ActivityLevel::Error, detail)
                    }
                    StreamStatus::Connecting => {}
                }
            }
            ConsoleMessage::ChartStreamStatus(status) => {
                self.chart_stream_status = StreamStatusLabel::from(status.clone());
                match status {
                    StreamStatus::Open => self.push_activity(
                        ActivityCategory::System,
                        ActivityLevel::Success,
                        "chart stream connected".to_owned(),
                    ),
                    StreamStatus::Closed => self.push_activity(
                        ActivityCategory::Warning,
                        ActivityLevel::Warning,
                        "chart stream closed".to_owned(),
                    ),
                    StreamStatus::Error(detail) => {
                        self.push_activity(ActivityCategory::Warning, ActivityLevel::Error, detail)
                    }
                    StreamStatus::Connecting => {}
                }
            }
        }
    }

    pub fn events_stream_url(&self) -> Option<String> {
        self.event_stream_url.clone()
    }

    pub fn chart_limit(&self) -> usize {
        self.selected_timeframe
            .map(|timeframe| chart_snapshot_limit(timeframe, DEFAULT_CHART_VIEWPORT_WIDTH))
            .unwrap_or(96)
    }

    pub fn chart_stream_url(&self) -> Option<String> {
        let timeframe = self.selected_timeframe?;
        let base = self.chart_stream_url_base.as_ref()?;
        let limit = self.chart_limit();
        Some(format!(
            "{base}?timeframe={}&limit={limit}",
            timeframe_query_value(timeframe)
        ))
    }

    pub fn event_stream_fingerprint(&self) -> Option<u64> {
        self.event_stream_fingerprint
    }

    pub fn chart_stream_fingerprint(&self) -> Option<u64> {
        self.chart_stream_fingerprint
    }

    pub fn pending_confirmation_prompt(&self) -> Option<&str> {
        self.pending_confirmation
            .as_ref()
            .map(|pending| pending.prompt.as_str())
    }

    pub fn show_help_overlay(&self) -> bool {
        self.show_help_overlay
    }

    pub fn toggle_help_overlay(&mut self) {
        self.show_help_overlay = !self.show_help_overlay;
    }

    pub fn dismiss_help_overlay(&mut self) {
        self.show_help_overlay = false;
    }

    #[cfg(test)]
    pub(crate) fn set_chart_snapshot_for_test(&mut self, snapshot: RuntimeChartSnapshot) {
        self.selected_timeframe = Some(snapshot.timeframe);
        self.chart_snapshot = Some(snapshot);
    }

    pub fn supported_timeframes(&self) -> Vec<Timeframe> {
        self.chart_config
            .as_ref()
            .map(|config| config.supported_timeframes.clone())
            .unwrap_or_default()
    }

    pub fn selected_timeframe(&self) -> Option<Timeframe> {
        self.selected_timeframe
    }

    pub fn request_set_mode(&self, mode: RuntimeMode) -> Option<ActionRequest> {
        let status = self.status.as_ref()?;
        if status.mode == mode {
            return None;
        }

        let prompt = if mode == RuntimeMode::Live {
            Some(
                "Switch the runtime into LIVE mode? Paper and live are intentionally separated."
                    .to_owned(),
            )
        } else {
            None
        };

        Some(ActionRequest {
            action: ConsoleAction::SetMode { mode },
            prompt,
        })
    }

    pub fn request_arm(&self) -> Option<ActionRequest> {
        let status = self.status.as_ref()?;
        if status.arm_state == tv_bot_core_types::ArmState::Armed {
            return None;
        }

        let allow_override = self
            .readiness
            .as_ref()
            .map(|readiness| readiness.report.hard_override_required)
            .unwrap_or(false);
        let prompt = if allow_override {
            Some("Arm now with a temporary override for this session?".to_owned())
        } else if status.mode == RuntimeMode::Live {
            Some("Arm LIVE trading? This enables live execution once commands or strategy logic fire.".to_owned())
        } else {
            None
        };

        Some(ActionRequest {
            action: ConsoleAction::Arm { allow_override },
            prompt,
        })
    }

    pub fn request_disarm(&self) -> Option<ActionRequest> {
        let status = self.status.as_ref()?;
        if status.arm_state == tv_bot_core_types::ArmState::Disarmed {
            return None;
        }

        Some(ActionRequest {
            action: ConsoleAction::Disarm,
            prompt: None,
        })
    }

    pub fn request_pause_resume(&self) -> Option<ActionRequest> {
        let status = self.status.as_ref()?;
        let action = if status.mode == RuntimeMode::Paused {
            ConsoleAction::Resume
        } else {
            ConsoleAction::Pause
        };

        Some(ActionRequest {
            action,
            prompt: None,
        })
    }

    pub fn select_timeframe(&mut self, timeframe: Timeframe) -> bool {
        let Some(chart_config) = &self.chart_config else {
            self.push_activity(
                ActivityCategory::Warning,
                ActivityLevel::Warning,
                "chart timeframes are not available yet".to_owned(),
            );
            return false;
        };

        if !chart_config.supported_timeframes.contains(&timeframe) {
            self.push_activity(
                ActivityCategory::Warning,
                ActivityLevel::Warning,
                format!(
                    "chart timeframe {} is not supported for the active contract",
                    timeframe_label(timeframe)
                ),
            );
            return false;
        }

        if self.selected_timeframe == Some(timeframe) {
            return false;
        }

        self.selected_timeframe = Some(timeframe);
        self.chart_stream_fingerprint = self
            .chart_stream_url()
            .as_ref()
            .map(|value| stream_fingerprint(value));
        self.push_activity(
            ActivityCategory::System,
            ActivityLevel::Info,
            format!("chart timeframe switched to {}", timeframe_label(timeframe)),
        );
        true
    }

    pub fn set_pending_confirmation(&mut self, action: ConsoleAction, prompt: String) {
        self.pending_confirmation = Some(PendingConfirmation { action, prompt });
    }

    pub fn pending_confirmation_action(&self) -> Option<ConsoleAction> {
        self.pending_confirmation
            .as_ref()
            .map(|pending| pending.action.clone())
    }

    pub fn clear_pending_confirmation(&mut self) {
        self.pending_confirmation = None;
    }

    pub fn has_pending_confirmation(&self) -> bool {
        self.pending_confirmation.is_some()
    }

    pub fn apply_lifecycle_response(&mut self, response: &RuntimeLifecycleResponse) {
        self.status = Some(response.status.clone());
        self.readiness = Some(response.readiness.clone());
        let level = match response.status_code {
            tv_bot_control_api::HttpStatusCode::Ok => ActivityLevel::Success,
            tv_bot_control_api::HttpStatusCode::Conflict
            | tv_bot_control_api::HttpStatusCode::PreconditionRequired => ActivityLevel::Warning,
            tv_bot_control_api::HttpStatusCode::InternalServerError => ActivityLevel::Error,
        };
        self.push_activity(ActivityCategory::Command, level, response.message.clone());

        if let Some(command_result) = &response.command_result {
            for warning in &command_result.warnings {
                self.push_activity(
                    ActivityCategory::Warning,
                    ActivityLevel::Warning,
                    format!("warning: {warning}"),
                );
            }
        }
    }

    pub fn note_activity(&mut self, entry: impl Into<String>) {
        self.push_activity(ActivityCategory::System, ActivityLevel::Info, entry.into());
    }

    fn last_refresh_summary(&self) -> String {
        match self.last_refresh_at {
            Some(instant) => {
                let seconds = instant.elapsed().as_secs();
                if seconds == 0 {
                    "just now".to_owned()
                } else {
                    format!("{seconds}s ago")
                }
            }
            None => "pending".to_owned(),
        }
    }

    fn apply_control_event(&mut self, event: ControlApiEvent) {
        match event {
            ControlApiEvent::CommandResult { result, .. } => {
                let (status_label, level) = match result.status {
                    ControlApiCommandStatus::Executed => ("executed", ActivityLevel::Success),
                    ControlApiCommandStatus::Rejected => ("rejected", ActivityLevel::Error),
                    ControlApiCommandStatus::RequiresOverride => {
                        ("override required", ActivityLevel::Warning)
                    }
                };
                self.push_activity(
                    ActivityCategory::Command,
                    level,
                    format!("command {status_label}: {}", result.reason),
                );
            }
            ControlApiEvent::ReadinessReport { report, .. } => {
                if let Some(readiness) = &mut self.readiness {
                    readiness.report = report.clone();
                }
                let category = if report.hard_override_required {
                    ActivityCategory::Warning
                } else {
                    ActivityCategory::System
                };
                let level = if report.hard_override_required {
                    ActivityLevel::Warning
                } else {
                    ActivityLevel::Info
                };
                self.push_activity(
                    category,
                    level,
                    format!("readiness: {}", report.risk_summary),
                );
            }
            ControlApiEvent::BrokerStatus { snapshot, .. } => {
                if let Some(status) = &mut self.status {
                    status.broker_status = Some(snapshot.clone());
                }
                let level = if matches!(
                    snapshot.health,
                    tv_bot_core_types::BrokerHealth::Degraded
                        | tv_bot_core_types::BrokerHealth::Disconnected
                        | tv_bot_core_types::BrokerHealth::Failed
                ) {
                    ActivityLevel::Warning
                } else {
                    ActivityLevel::Info
                };
                self.push_activity(
                    ActivityCategory::System,
                    level,
                    format!(
                        "broker {:?} / {:?} ({:?})",
                        snapshot.connection_state, snapshot.sync_state, snapshot.health
                    ),
                );
            }
            ControlApiEvent::SystemHealth { snapshot, .. } => {
                if let Some(status) = &mut self.status {
                    status.system_health = Some(snapshot.clone());
                }
                let level = if snapshot.feed_degraded || snapshot.error_count > 0 {
                    ActivityLevel::Warning
                } else {
                    ActivityLevel::Info
                };
                self.push_activity(
                    ActivityCategory::System,
                    level,
                    format!(
                        "system health updated: errors={} feed_degraded={}",
                        snapshot.error_count, snapshot.feed_degraded
                    ),
                );
            }
            ControlApiEvent::TradeLatency { record, .. } => {
                if let Some(status) = &mut self.status {
                    status.latest_trade_latency = Some(record.clone());
                }
                self.push_activity(
                    ActivityCategory::System,
                    ActivityLevel::Info,
                    format!(
                        "trade latency recorded: action={} fill_ms={:?}",
                        record.action_id, record.latency.end_to_end_fill_latency_ms
                    ),
                );
            }
            ControlApiEvent::HistorySnapshot { projection, .. } => {
                if let Some(history) = &mut self.history {
                    history.projection = projection;
                } else {
                    self.history = Some(RuntimeHistorySnapshot { projection });
                }
                self.push_history_activity();
            }
            ControlApiEvent::JournalRecord { record } => {
                self.push_activity(
                    ActivityCategory::System,
                    ActivityLevel::Info,
                    format!(
                        "journal {}:{} ({})",
                        record.category,
                        record.action,
                        action_source_label(record.source)
                    ),
                );
            }
        }
    }

    fn apply_chart_event(&mut self, event: RuntimeChartStreamEvent) {
        match event {
            RuntimeChartStreamEvent::Snapshot { snapshot, .. } => {
                self.selected_timeframe = Some(snapshot.timeframe);
                self.chart_config = Some(snapshot.config.clone());
                self.chart_snapshot = Some(snapshot);
            }
        }
    }

    fn push_baseline_activity(&mut self) {
        self.push_history_activity();

        if let Some(journal) = &self.journal {
            let recent_records: Vec<(String, String, ActionSource)> = journal
                .records
                .iter()
                .take(2)
                .map(|record| {
                    (
                        record.category.clone(),
                        record.action.clone(),
                        record.source,
                    )
                })
                .collect();

            for (category, action, source) in recent_records.into_iter().rev() {
                self.push_activity(
                    ActivityCategory::System,
                    ActivityLevel::Info,
                    format!(
                        "journal {}:{} ({})",
                        category,
                        action,
                        action_source_label(source)
                    ),
                );
            }
        }

        if let Some(readiness) = &self.readiness {
            if readiness.report.hard_override_required {
                self.push_activity(
                    ActivityCategory::Warning,
                    ActivityLevel::Warning,
                    format!("readiness warning: {}", readiness.report.risk_summary),
                );
            }
        }

        if let Some(status) = &self.status {
            let reconnect_required = status.reconnect_review.required;
            let reconnect_reason = status.reconnect_review.reason.clone();
            let shutdown_pending =
                status.shutdown_review.pending_signal || status.shutdown_review.blocked;
            let shutdown_reason = status.shutdown_review.reason.clone();

            if reconnect_required {
                self.push_activity(
                    ActivityCategory::Warning,
                    ActivityLevel::Warning,
                    format!(
                        "reconnect review required: {}",
                        reconnect_reason
                            .as_deref()
                            .unwrap_or("operator review required")
                    ),
                );
            }

            if shutdown_pending {
                self.push_activity(
                    ActivityCategory::Warning,
                    ActivityLevel::Warning,
                    format!(
                        "shutdown review pending: {}",
                        shutdown_reason
                            .as_deref()
                            .unwrap_or("operator decision required")
                    ),
                );
            }
        }
    }

    fn push_history_activity(&mut self) {
        let Some(history) = &self.history else {
            return;
        };

        let latest_fill = history.projection.latest_fill.as_ref().map(|fill| {
            (
                fill.fill_id.clone(),
                format!(
                    "fill {} {:?} x{} @ {}",
                    fill.symbol, fill.side, fill.quantity, fill.price
                ),
            )
        });
        let latest_order = history.projection.latest_order.as_ref().map(|order| {
            (
                order.broker_order_id.clone(),
                format!(
                    "order {} {:?} x{} {:?}",
                    order.symbol, order.side, order.quantity, order.status
                ),
            )
        });

        if let Some((fill_id, message)) = latest_fill {
            if self.last_fill_id.as_deref() != Some(fill_id.as_str()) {
                self.last_fill_id = Some(fill_id);
                self.push_activity(ActivityCategory::Trade, ActivityLevel::Success, message);
            }
        }

        if let Some((order_id, message)) = latest_order {
            if self.last_order_id.as_deref() != Some(order_id.as_str()) {
                self.last_order_id = Some(order_id);
                self.push_activity(ActivityCategory::Trade, ActivityLevel::Info, message);
            }
        }
    }

    fn push_activity(&mut self, category: ActivityCategory, level: ActivityLevel, entry: String) {
        if self
            .activity_feed
            .front()
            .is_some_and(|current| current.message == entry)
        {
            return;
        }

        self.activity_feed.push_front(ActivityEntry {
            category,
            level,
            message: entry,
        });
        while self.activity_feed.len() > MAX_ACTIVITY_ITEMS {
            self.activity_feed.pop_back();
        }
    }
}

fn labeled(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::raw(value.to_owned()),
    ])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityCategory {
    Trade,
    Command,
    Warning,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityFilter {
    All,
    Warnings,
    Trades,
    Commands,
    System,
}

impl ActivityFilter {
    fn label(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Warnings => "warnings",
            Self::Trades => "trades",
            Self::Commands => "commands",
            Self::System => "system",
        }
    }

    fn matches(&self, category: ActivityCategory) -> bool {
        match self {
            Self::All => true,
            Self::Warnings => category == ActivityCategory::Warning,
            Self::Trades => category == ActivityCategory::Trade,
            Self::Commands => category == ActivityCategory::Command,
            Self::System => category == ActivityCategory::System,
        }
    }

    fn next(&self) -> Self {
        match self {
            Self::All => Self::Warnings,
            Self::Warnings => Self::Trades,
            Self::Trades => Self::Commands,
            Self::Commands => Self::System,
            Self::System => Self::All,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActivityEntry {
    category: ActivityCategory,
    level: ActivityLevel,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEntryView {
    pub label: String,
    pub message: String,
    pub level: ActivityLevel,
}

impl From<&ActivityEntry> for ActivityEntryView {
    fn from(value: &ActivityEntry) -> Self {
        let label = match value.category {
            ActivityCategory::Trade => "trade",
            ActivityCategory::Command => "cmd",
            ActivityCategory::Warning => "warn",
            ActivityCategory::System => "sys",
        };
        Self {
            label: label.to_owned(),
            message: value.message.clone(),
            level: value.level,
        }
    }
}

#[derive(Clone, Debug)]
enum StreamStatusLabel {
    Idle,
    Connecting,
    Open,
    Closed,
    Error(String),
}

impl StreamStatusLabel {
    fn as_str(&self) -> &str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Error(detail) => detail.as_str(),
        }
    }
}

impl From<StreamStatus> for StreamStatusLabel {
    fn from(value: StreamStatus) -> Self {
        match value {
            StreamStatus::Connecting => Self::Connecting,
            StreamStatus::Open => Self::Open,
            StreamStatus::Closed => Self::Closed,
            StreamStatus::Error(detail) => Self::Error(detail),
        }
    }
}

struct RefreshData {
    status: RuntimeStatusSnapshot,
    readiness: RuntimeReadinessSnapshot,
    history: RuntimeHistorySnapshot,
    journal: RuntimeJournalSnapshot,
    chart_config: RuntimeChartConfigResponse,
    chart_snapshot: Option<RuntimeChartSnapshot>,
    selected_timeframe: Option<Timeframe>,
    event_stream_url: Option<String>,
    chart_stream_url_base: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConsoleAction {
    SetMode { mode: RuntimeMode },
    Arm { allow_override: bool },
    Disarm,
    Pause,
    Resume,
}

impl ConsoleAction {
    pub fn to_lifecycle_command(&self) -> RuntimeLifecycleCommand {
        match self {
            Self::SetMode { mode } => RuntimeLifecycleCommand::SetMode { mode: mode.clone() },
            Self::Arm { allow_override } => RuntimeLifecycleCommand::Arm {
                allow_override: *allow_override,
            },
            Self::Disarm => RuntimeLifecycleCommand::Disarm,
            Self::Pause => RuntimeLifecycleCommand::Pause,
            Self::Resume => RuntimeLifecycleCommand::Resume,
        }
    }
}

pub struct ActionRequest {
    pub action: ConsoleAction,
    pub prompt: Option<String>,
}

#[derive(Clone)]
struct PendingConfirmation {
    action: ConsoleAction,
    prompt: String,
}

async fn refresh_snapshot(
    client: &Client,
    base_url: &str,
    preferred_timeframe: Option<Timeframe>,
) -> Result<RefreshData, Box<dyn std::error::Error>> {
    let status = fetch_status(client, base_url).await?;
    let readiness = fetch_readiness(client, base_url).await?;
    let history = fetch_history(client, base_url).await?;
    let journal = fetch_journal(client, base_url).await?;
    let chart_config = fetch_chart_config(client, base_url).await?;
    let selected_timeframe = choose_timeframe(&chart_config, preferred_timeframe);
    let chart_snapshot = if chart_config.available {
        if let Some(timeframe) = selected_timeframe {
            Some(
                fetch_chart_snapshot(
                    client,
                    base_url,
                    timeframe,
                    chart_snapshot_limit(timeframe, DEFAULT_CHART_VIEWPORT_WIDTH),
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };

    Ok(RefreshData {
        event_stream_url: Some(websocket_url_from_bind(
            status.websocket_bind.as_str(),
            "/events",
        )),
        chart_stream_url_base: Some(websocket_url_from_bind(
            status.websocket_bind.as_str(),
            "/chart/stream",
        )),
        status,
        readiness,
        history,
        journal,
        chart_config,
        chart_snapshot,
        selected_timeframe,
    })
}

fn websocket_url_from_bind(bind: &str, path: &str) -> String {
    format!("ws://{}{}", bind.trim_end_matches('/'), path)
}

fn choose_timeframe(
    chart_config: &RuntimeChartConfigResponse,
    preferred_timeframe: Option<Timeframe>,
) -> Option<Timeframe> {
    preferred_timeframe
        .filter(|timeframe| chart_config.supported_timeframes.contains(timeframe))
        .or(chart_config.default_timeframe)
        .or_else(|| chart_config.supported_timeframes.first().copied())
}

fn stream_fingerprint(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn action_source_label(source: ActionSource) -> &'static str {
    match source {
        ActionSource::Dashboard => "dashboard",
        ActionSource::Cli => "cli",
        ActionSource::System => "system",
    }
}

fn decimal_to_f64<T: ToString>(value: &T) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}

fn horizontal_line(bounds: [f64; 2], price: f64) -> Vec<(f64, f64)> {
    vec![(bounds[0], price), (bounds[1], price)]
}

fn nearest_fill_marker_point(
    snapshot: &RuntimeChartSnapshot,
    fill: &tv_bot_core_types::BrokerFillUpdate,
) -> (f64, f64) {
    let mut selected_index = 0usize;

    for (index, bar) in snapshot.bars.iter().enumerate() {
        if bar.closed_at > fill.occurred_at {
            break;
        }
        selected_index = index;
    }

    (selected_index as f64, decimal_to_f64(&fill.price))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_reports_waiting_activity() {
        let state = ConsoleState::new(Duration::from_secs(5));

        assert_eq!(
            state.activity_items(),
            vec![ActivityEntryView {
                label: "idle".to_owned(),
                message: "waiting for runtime activity from the local host".to_owned(),
                level: ActivityLevel::Info,
            }]
        );
    }

    #[test]
    fn footer_reports_refresh_interval() {
        let state = ConsoleState::new(Duration::from_secs(7));

        assert_eq!(
            state.footer_status(),
            "auto refresh every 7s | events idle | chart idle"
        );
    }

    #[test]
    fn websocket_url_uses_runtime_bind() {
        assert_eq!(
            websocket_url_from_bind("127.0.0.1:8081", "/events"),
            "ws://127.0.0.1:8081/events"
        );
    }

    #[test]
    fn live_mode_requires_confirmation() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.status = Some(RuntimeStatusSnapshot {
            mode: RuntimeMode::Paper,
            arm_state: tv_bot_core_types::ArmState::Disarmed,
            warmup_status: tv_bot_core_types::WarmupStatus::Ready,
            strategy_loaded: false,
            hard_override_active: false,
            operator_new_entries_enabled: true,
            operator_new_entries_reason: None,
            current_strategy: None,
            broker_status: None,
            market_data_status: None,
            market_data_detail: None,
            storage_status: tv_bot_control_api::RuntimeStorageStatus {
                mode: tv_bot_control_api::RuntimeStorageMode::Unconfigured,
                primary_configured: false,
                sqlite_fallback_enabled: false,
                sqlite_path: std::path::PathBuf::new(),
                allow_runtime_fallback: false,
                active_backend: "none".to_owned(),
                durable: false,
                fallback_activated: false,
                detail: "none".to_owned(),
            },
            journal_status: tv_bot_control_api::RuntimeJournalStatus {
                backend: "none".to_owned(),
                durable: false,
                detail: "none".to_owned(),
            },
            system_health: None,
            latest_trade_latency: None,
            recorded_trade_latency_count: 0,
            current_account_name: None,
            instrument_mapping: None,
            instrument_resolution_error: None,
            reconnect_review: tv_bot_control_api::RuntimeReconnectReviewStatus {
                required: false,
                reason: None,
                last_decision: None,
                open_position_count: 0,
                working_order_count: 0,
            },
            shutdown_review: tv_bot_control_api::RuntimeShutdownReviewStatus {
                pending_signal: false,
                blocked: false,
                awaiting_flatten: false,
                decision: None,
                reason: None,
                open_position_count: 0,
                all_positions_broker_protected: false,
            },
            http_bind: "127.0.0.1:8080".to_owned(),
            websocket_bind: "127.0.0.1:8081".to_owned(),
            command_dispatch_ready: true,
            command_dispatch_detail: "ready".to_owned(),
        });

        let request = state
            .request_set_mode(RuntimeMode::Live)
            .expect("should create request");

        assert!(request.prompt.is_some());
    }

    #[test]
    fn choose_timeframe_prefers_supported_selection() {
        let chart_config = RuntimeChartConfigResponse {
            available: true,
            detail: "ready".to_owned(),
            sample_data_active: false,
            instrument: None,
            supported_timeframes: vec![Timeframe::OneMinute, Timeframe::FiveMinute],
            default_timeframe: Some(Timeframe::OneMinute),
            market_data_connection_state: None,
            market_data_health: None,
            replay_caught_up: true,
            trade_ready: true,
        };

        assert_eq!(
            choose_timeframe(&chart_config, Some(Timeframe::FiveMinute)),
            Some(Timeframe::FiveMinute)
        );
        assert_eq!(
            choose_timeframe(&chart_config, Some(Timeframe::OneSecond)),
            Some(Timeframe::OneMinute)
        );
    }

    #[test]
    fn select_timeframe_updates_fingerprint() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.chart_config = Some(RuntimeChartConfigResponse {
            available: true,
            detail: "ready".to_owned(),
            sample_data_active: false,
            instrument: None,
            supported_timeframes: vec![Timeframe::OneMinute, Timeframe::FiveMinute],
            default_timeframe: Some(Timeframe::OneMinute),
            market_data_connection_state: None,
            market_data_health: None,
            replay_caught_up: true,
            trade_ready: true,
        });
        state.chart_stream_url_base = Some("ws://127.0.0.1:8081/chart/stream".to_owned());
        state.selected_timeframe = Some(Timeframe::OneMinute);
        state.chart_stream_fingerprint = state
            .chart_stream_url()
            .as_ref()
            .map(|value| stream_fingerprint(value));

        let previous_fingerprint = state.chart_stream_fingerprint();

        assert!(state.select_timeframe(Timeframe::FiveMinute));
        assert_ne!(state.chart_stream_fingerprint(), previous_fingerprint);
        assert_eq!(state.selected_timeframe(), Some(Timeframe::FiveMinute));
    }

    #[test]
    fn pending_confirmation_prompt_is_available() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.set_pending_confirmation(ConsoleAction::Disarm, "Confirm disarm?".to_owned());

        assert_eq!(state.pending_confirmation_prompt(), Some("Confirm disarm?"));
    }

    #[test]
    fn help_overlay_toggles_and_dismisses() {
        let mut state = ConsoleState::new(Duration::from_secs(5));

        assert!(!state.show_help_overlay());
        state.toggle_help_overlay();
        assert!(state.show_help_overlay());
        state.dismiss_help_overlay();
        assert!(!state.show_help_overlay());
    }

    #[test]
    fn activity_filter_cycles_and_filters_entries() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.push_activity(
            ActivityCategory::Trade,
            ActivityLevel::Success,
            "fill nq buy".to_owned(),
        );
        state.push_activity(
            ActivityCategory::Warning,
            ActivityLevel::Warning,
            "feed degraded".to_owned(),
        );

        assert_eq!(state.activity_title(), "Recent Activity [all]");
        assert_eq!(state.activity_items().len(), 2);

        state.cycle_activity_filter();
        assert_eq!(state.activity_title(), "Recent Activity [warnings]");
        assert_eq!(state.activity_items()[0].message, "feed degraded");

        state.cycle_activity_filter();
        assert_eq!(state.activity_title(), "Recent Activity [trades]");
        assert_eq!(state.activity_items()[0].message, "fill nq buy");
    }

    #[test]
    fn chart_bounds_pad_visible_range() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.chart_snapshot = Some(RuntimeChartSnapshot {
            config: RuntimeChartConfigResponse {
                available: true,
                detail: "ready".to_owned(),
                sample_data_active: false,
                instrument: None,
                supported_timeframes: vec![Timeframe::OneMinute],
                default_timeframe: Some(Timeframe::OneMinute),
                market_data_connection_state: None,
                market_data_health: None,
                replay_caught_up: true,
                trade_ready: true,
            },
            timeframe: Timeframe::OneMinute,
            requested_limit: 3,
            bars: vec![
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.00".parse().unwrap(),
                    high: "10.10".parse().unwrap(),
                    low: "9.95".parse().unwrap(),
                    close: "10.05".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:30:00Z".parse().unwrap(),
                    is_complete: true,
                },
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.05".parse().unwrap(),
                    high: "10.25".parse().unwrap(),
                    low: "10.00".parse().unwrap(),
                    close: "10.20".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:31:00Z".parse().unwrap(),
                    is_complete: true,
                },
            ],
            latest_price: Some("10.20".parse().unwrap()),
            latest_closed_at: Some("2026-04-18T09:31:00Z".parse().unwrap()),
            active_position: None,
            working_orders: vec![],
            recent_fills: vec![],
            can_load_older_history: false,
        });

        let bounds = state.chart_y_bounds();

        assert!(bounds[0] < 9.95);
        assert!(bounds[1] > 10.25);
    }

    #[test]
    fn chart_labels_follow_bar_times() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.chart_snapshot = Some(RuntimeChartSnapshot {
            config: RuntimeChartConfigResponse {
                available: true,
                detail: "ready".to_owned(),
                sample_data_active: false,
                instrument: None,
                supported_timeframes: vec![Timeframe::FiveMinute],
                default_timeframe: Some(Timeframe::FiveMinute),
                market_data_connection_state: None,
                market_data_health: None,
                replay_caught_up: true,
                trade_ready: true,
            },
            timeframe: Timeframe::FiveMinute,
            requested_limit: 2,
            bars: vec![
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::FiveMinute,
                    open: "10.00".parse().unwrap(),
                    high: "10.10".parse().unwrap(),
                    low: "9.95".parse().unwrap(),
                    close: "10.05".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T14:05:00Z".parse().unwrap(),
                    is_complete: true,
                },
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::FiveMinute,
                    open: "10.05".parse().unwrap(),
                    high: "10.25".parse().unwrap(),
                    low: "10.00".parse().unwrap(),
                    close: "10.20".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T14:10:00Z".parse().unwrap(),
                    is_complete: true,
                },
            ],
            latest_price: Some("10.20".parse().unwrap()),
            latest_closed_at: Some("2026-04-18T14:10:00Z".parse().unwrap()),
            active_position: None,
            working_orders: vec![],
            recent_fills: vec![],
            can_load_older_history: false,
        });

        assert_eq!(
            state.chart_x_labels(),
            vec!["14:05".to_owned(), "14:10".to_owned()]
        );
        assert_eq!(state.chart_x_bounds(), [0.0, 1.0]);
    }

    #[test]
    fn chart_guides_follow_latest_price_and_position() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.chart_snapshot = Some(RuntimeChartSnapshot {
            config: RuntimeChartConfigResponse {
                available: true,
                detail: "ready".to_owned(),
                sample_data_active: false,
                instrument: None,
                supported_timeframes: vec![Timeframe::OneMinute],
                default_timeframe: Some(Timeframe::OneMinute),
                market_data_connection_state: None,
                market_data_health: None,
                replay_caught_up: true,
                trade_ready: true,
            },
            timeframe: Timeframe::OneMinute,
            requested_limit: 2,
            bars: vec![
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.00".parse().unwrap(),
                    high: "10.10".parse().unwrap(),
                    low: "9.95".parse().unwrap(),
                    close: "10.05".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:30:00Z".parse().unwrap(),
                    is_complete: true,
                },
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.05".parse().unwrap(),
                    high: "10.25".parse().unwrap(),
                    low: "10.00".parse().unwrap(),
                    close: "10.20".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:31:00Z".parse().unwrap(),
                    is_complete: true,
                },
            ],
            latest_price: Some("10.20".parse().unwrap()),
            latest_closed_at: Some("2026-04-18T09:31:00Z".parse().unwrap()),
            active_position: Some(tv_bot_core_types::BrokerPositionSnapshot {
                account_id: None,
                symbol: "NQ".to_owned(),
                quantity: 1,
                average_price: Some("10.12".parse().unwrap()),
                realized_pnl: None,
                unrealized_pnl: None,
                protective_orders_present: true,
                captured_at: "2026-04-18T09:31:00Z".parse().unwrap(),
            }),
            working_orders: vec![],
            recent_fills: vec![],
            can_load_older_history: false,
        });

        assert_eq!(
            state.chart_current_price_line(),
            vec![(0.0, 10.2), (1.0, 10.2)]
        );
        assert_eq!(
            state.chart_active_position_line(),
            vec![(0.0, 10.12), (1.0, 10.12)]
        );
    }

    #[test]
    fn fill_markers_snap_to_nearest_completed_bar() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.chart_snapshot = Some(RuntimeChartSnapshot {
            config: RuntimeChartConfigResponse {
                available: true,
                detail: "ready".to_owned(),
                sample_data_active: false,
                instrument: None,
                supported_timeframes: vec![Timeframe::OneMinute],
                default_timeframe: Some(Timeframe::OneMinute),
                market_data_connection_state: None,
                market_data_health: None,
                replay_caught_up: true,
                trade_ready: true,
            },
            timeframe: Timeframe::OneMinute,
            requested_limit: 3,
            bars: vec![
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.00".parse().unwrap(),
                    high: "10.10".parse().unwrap(),
                    low: "9.95".parse().unwrap(),
                    close: "10.05".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:30:00Z".parse().unwrap(),
                    is_complete: true,
                },
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.05".parse().unwrap(),
                    high: "10.25".parse().unwrap(),
                    low: "10.00".parse().unwrap(),
                    close: "10.20".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:31:00Z".parse().unwrap(),
                    is_complete: true,
                },
                tv_bot_control_api::RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.20".parse().unwrap(),
                    high: "10.30".parse().unwrap(),
                    low: "10.10".parse().unwrap(),
                    close: "10.15".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:32:00Z".parse().unwrap(),
                    is_complete: false,
                },
            ],
            latest_price: Some("10.15".parse().unwrap()),
            latest_closed_at: Some("2026-04-18T09:32:00Z".parse().unwrap()),
            active_position: None,
            working_orders: vec![],
            recent_fills: vec![
                tv_bot_core_types::BrokerFillUpdate {
                    fill_id: "buy-1".to_owned(),
                    broker_order_id: None,
                    account_id: None,
                    symbol: "NQ".to_owned(),
                    side: tv_bot_core_types::TradeSide::Buy,
                    quantity: 1,
                    price: "10.08".parse().unwrap(),
                    fee: None,
                    commission: None,
                    occurred_at: "2026-04-18T09:30:30Z".parse().unwrap(),
                },
                tv_bot_core_types::BrokerFillUpdate {
                    fill_id: "sell-1".to_owned(),
                    broker_order_id: None,
                    account_id: None,
                    symbol: "NQ".to_owned(),
                    side: tv_bot_core_types::TradeSide::Sell,
                    quantity: 1,
                    price: "10.18".parse().unwrap(),
                    fee: None,
                    commission: None,
                    occurred_at: "2026-04-18T09:31:15Z".parse().unwrap(),
                },
            ],
            can_load_older_history: false,
        });

        let (buys, sells) = state.chart_fill_marker_points();

        assert_eq!(buys, vec![(0.0, 10.08)]);
        assert_eq!(sells, vec![(1.0, 10.18)]);
    }

    #[test]
    fn chart_points_follow_close_values() {
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.chart_snapshot = Some(RuntimeChartSnapshot {
            config: RuntimeChartConfigResponse {
                available: true,
                detail: "ready".to_owned(),
                sample_data_active: false,
                instrument: None,
                supported_timeframes: vec![Timeframe::OneMinute],
                default_timeframe: Some(Timeframe::OneMinute),
                market_data_connection_state: None,
                market_data_health: None,
                replay_caught_up: true,
                trade_ready: true,
            },
            timeframe: Timeframe::OneMinute,
            requested_limit: 1,
            bars: vec![tv_bot_control_api::RuntimeChartBar {
                timeframe: Timeframe::OneMinute,
                open: "10.00".parse().unwrap(),
                high: "10.25".parse().unwrap(),
                low: "9.95".parse().unwrap(),
                close: "10.20".parse().unwrap(),
                volume: 1,
                closed_at: "2026-04-18T09:31:00Z".parse().unwrap(),
                is_complete: true,
            }],
            latest_price: Some("10.20".parse().unwrap()),
            latest_closed_at: Some("2026-04-18T09:31:00Z".parse().unwrap()),
            active_position: None,
            working_orders: vec![],
            recent_fills: vec![],
            can_load_older_history: false,
        });

        assert_eq!(state.chart_points(), vec![(0.0, 10.2)]);
    }
}
