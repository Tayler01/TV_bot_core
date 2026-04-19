use std::{
    collections::hash_map::DefaultHasher,
    error::Error,
    hash::{Hash, Hasher},
    io::{self, Stdout},
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use reqwest::Client;
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};
use tokio_tungstenite::connect_async;
use tv_bot_control_api::{ControlApiEvent, RuntimeChartStreamEvent};
use tv_bot_core_types::{RuntimeMode, Timeframe};

use crate::commands::execute_lifecycle_command;

use super::{
    render::render_console,
    state::{ActionRequest, ConsoleAction, ConsoleState},
};

type ConsoleResult<T = ()> = Result<T, Box<dyn Error>>;

const STREAM_RECONNECT_DELAY: Duration = Duration::from_millis(1_500);

pub async fn run_console(
    client: &Client,
    base_url: &str,
    refresh_interval: Duration,
) -> ConsoleResult {
    let mut terminal = TerminalSession::enter()?;
    let mut state = ConsoleState::new(refresh_interval);
    let (message_tx, mut message_rx) = mpsc::unbounded_channel();
    let mut event_stream_task: Option<StreamTask> = None;
    let mut chart_stream_task: Option<StreamTask> = None;

    state.refresh(client, base_url).await;
    ensure_event_stream(&state, &message_tx, &mut event_stream_task);
    ensure_chart_stream(&state, &message_tx, &mut chart_stream_task);

    loop {
        while let Ok(message) = message_rx.try_recv() {
            state.apply_message(message);
        }

        terminal
            .terminal
            .draw(|frame| render_console(frame, &state))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('h') | KeyCode::Char('?')
                        if !state.has_pending_confirmation() =>
                    {
                        state.toggle_help_overlay();
                    }
                    KeyCode::Char('y') if state.has_pending_confirmation() => {
                        if let Some(action) = state.pending_confirmation_action() {
                            state.clear_pending_confirmation();
                            execute_console_action(action, &mut state, client, base_url).await;
                            ensure_event_stream(&state, &message_tx, &mut event_stream_task);
                            ensure_chart_stream(&state, &message_tx, &mut chart_stream_task);
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Esc if state.has_pending_confirmation() => {
                        state.clear_pending_confirmation();
                        state.note_activity("confirmation cancelled");
                    }
                    KeyCode::Esc if state.show_help_overlay() => {
                        state.dismiss_help_overlay();
                    }
                    KeyCode::Char('p')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_action_request(
                            state.request_set_mode(RuntimeMode::Paper),
                            &mut state,
                            client,
                            base_url,
                        )
                        .await;
                    }
                    KeyCode::Char('o')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_action_request(
                            state.request_set_mode(RuntimeMode::Observation),
                            &mut state,
                            client,
                            base_url,
                        )
                        .await;
                    }
                    KeyCode::Char('l')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_action_request(
                            state.request_set_mode(RuntimeMode::Live),
                            &mut state,
                            client,
                            base_url,
                        )
                        .await;
                    }
                    KeyCode::Char('a')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_action_request(state.request_arm(), &mut state, client, base_url)
                            .await;
                    }
                    KeyCode::Char('d')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_action_request(state.request_disarm(), &mut state, client, base_url)
                            .await;
                    }
                    KeyCode::Char(' ')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_action_request(
                            state.request_pause_resume(),
                            &mut state,
                            client,
                            base_url,
                        )
                        .await;
                    }
                    KeyCode::Char('1')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_timeframe_change(
                            Timeframe::OneSecond,
                            &mut state,
                            client,
                            base_url,
                            &message_tx,
                            &mut chart_stream_task,
                        )
                        .await;
                    }
                    KeyCode::Char('2')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_timeframe_change(
                            Timeframe::OneMinute,
                            &mut state,
                            client,
                            base_url,
                            &message_tx,
                            &mut chart_stream_task,
                        )
                        .await;
                    }
                    KeyCode::Char('3')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        handle_timeframe_change(
                            Timeframe::FiveMinute,
                            &mut state,
                            client,
                            base_url,
                            &message_tx,
                            &mut chart_stream_task,
                        )
                        .await;
                    }
                    KeyCode::Char('r') if !state.show_help_overlay() => {
                        state.refresh(client, base_url).await;
                        ensure_event_stream(&state, &message_tx, &mut event_stream_task);
                        ensure_chart_stream(&state, &message_tx, &mut chart_stream_task);
                    }
                    KeyCode::Char('f')
                        if !state.has_pending_confirmation() && !state.show_help_overlay() =>
                    {
                        state.cycle_activity_filter();
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        if state.refresh_due() {
            state.refresh(client, base_url).await;
            ensure_event_stream(&state, &message_tx, &mut event_stream_task);
            ensure_chart_stream(&state, &message_tx, &mut chart_stream_task);
        }
    }

    if let Some(task) = event_stream_task.take() {
        task.handle.abort();
    }
    if let Some(task) = chart_stream_task.take() {
        task.handle.abort();
    }

    Ok(())
}

async fn handle_action_request(
    request: Option<ActionRequest>,
    state: &mut ConsoleState,
    client: &Client,
    base_url: &str,
) {
    let Some(request) = request else {
        return;
    };

    if let Some(prompt) = request.prompt {
        state.set_pending_confirmation(request.action, prompt);
        return;
    }

    execute_console_action(request.action, state, client, base_url).await;
}

async fn execute_console_action(
    action: ConsoleAction,
    state: &mut ConsoleState,
    client: &Client,
    base_url: &str,
) {
    match execute_lifecycle_command(client, base_url, action.to_lifecycle_command()).await {
        Ok(response) => {
            state.apply_lifecycle_response(&response);
            state.refresh(client, base_url).await;
        }
        Err(error) => {
            state.note_activity(format!("command failed: {error}"));
        }
    }
}

async fn handle_timeframe_change(
    timeframe: Timeframe,
    state: &mut ConsoleState,
    client: &Client,
    base_url: &str,
    sender: &mpsc::UnboundedSender<ConsoleMessage>,
    task: &mut Option<StreamTask>,
) {
    if !state.select_timeframe(timeframe) {
        return;
    }

    state.refresh_chart(client, base_url).await;
    ensure_chart_stream(state, sender, task);
}

fn ensure_event_stream(
    state: &ConsoleState,
    sender: &mpsc::UnboundedSender<ConsoleMessage>,
    task: &mut Option<StreamTask>,
) {
    let Some(url) = state.events_stream_url() else {
        return;
    };
    let Some(fingerprint) = state.event_stream_fingerprint() else {
        return;
    };

    if task.as_ref().is_some_and(|stream_task| {
        !stream_task.handle.is_finished() && stream_task.fingerprint == fingerprint
    }) {
        return;
    }

    if let Some(stream_task) = task.take() {
        stream_task.handle.abort();
    }

    let sender = sender.clone();
    let handle = tokio::spawn(async move {
        stream_control_events(url, sender).await;
    });
    *task = Some(StreamTask {
        fingerprint,
        handle,
    });
}

fn ensure_chart_stream(
    state: &ConsoleState,
    sender: &mpsc::UnboundedSender<ConsoleMessage>,
    task: &mut Option<StreamTask>,
) {
    let Some(url) = state.chart_stream_url() else {
        return;
    };
    let Some(fingerprint) = state.chart_stream_fingerprint() else {
        return;
    };

    if task.as_ref().is_some_and(|stream_task| {
        !stream_task.handle.is_finished() && stream_task.fingerprint == fingerprint
    }) {
        return;
    }

    if let Some(stream_task) = task.take() {
        stream_task.handle.abort();
    }

    let sender = sender.clone();
    let handle = tokio::spawn(async move {
        stream_chart_events(url, sender).await;
    });
    *task = Some(StreamTask {
        fingerprint,
        handle,
    });
}

async fn stream_control_events(url: String, sender: mpsc::UnboundedSender<ConsoleMessage>) {
    let _ = sender.send(ConsoleMessage::EventStreamStatus(StreamStatus::Connecting));

    loop {
        match connect_async(url.as_str()).await {
            Ok((mut stream, _)) => {
                let _ = sender.send(ConsoleMessage::EventStreamStatus(StreamStatus::Open));

                while let Some(message) = stream.next().await {
                    match message {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(payload)) => {
                            match serde_json::from_str::<ControlApiEvent>(&payload) {
                                Ok(event) => {
                                    let _ = sender.send(ConsoleMessage::ControlEvent(event));
                                }
                                Err(error) => {
                                    let _ = sender.send(ConsoleMessage::EventStreamStatus(
                                        StreamStatus::Error(format!("event parse failed: {error}")),
                                    ));
                                }
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                        Ok(_) => {}
                        Err(error) => {
                            let _ = sender.send(ConsoleMessage::EventStreamStatus(
                                StreamStatus::Error(format!("event stream error: {error}")),
                            ));
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(ConsoleMessage::EventStreamStatus(StreamStatus::Error(
                    format!("event connect failed: {error}"),
                )));
            }
        }

        let _ = sender.send(ConsoleMessage::EventStreamStatus(StreamStatus::Closed));
        sleep(STREAM_RECONNECT_DELAY).await;
        let _ = sender.send(ConsoleMessage::EventStreamStatus(StreamStatus::Connecting));
    }
}

async fn stream_chart_events(url: String, sender: mpsc::UnboundedSender<ConsoleMessage>) {
    let _ = sender.send(ConsoleMessage::ChartStreamStatus(StreamStatus::Connecting));

    loop {
        match connect_async(url.as_str()).await {
            Ok((mut stream, _)) => {
                let _ = sender.send(ConsoleMessage::ChartStreamStatus(StreamStatus::Open));

                while let Some(message) = stream.next().await {
                    match message {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(payload)) => {
                            match serde_json::from_str::<RuntimeChartStreamEvent>(&payload) {
                                Ok(event) => {
                                    let _ = sender.send(ConsoleMessage::ChartEvent(event));
                                }
                                Err(error) => {
                                    let _ = sender.send(ConsoleMessage::ChartStreamStatus(
                                        StreamStatus::Error(format!("chart parse failed: {error}")),
                                    ));
                                }
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                        Ok(_) => {}
                        Err(error) => {
                            let _ = sender.send(ConsoleMessage::ChartStreamStatus(
                                StreamStatus::Error(format!("chart stream error: {error}")),
                            ));
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(ConsoleMessage::ChartStreamStatus(StreamStatus::Error(
                    format!("chart connect failed: {error}"),
                )));
            }
        }

        let _ = sender.send(ConsoleMessage::ChartStreamStatus(StreamStatus::Closed));
        sleep(STREAM_RECONNECT_DELAY).await;
        let _ = sender.send(ConsoleMessage::ChartStreamStatus(StreamStatus::Connecting));
    }
}

#[derive(Debug)]
pub enum ConsoleMessage {
    ControlEvent(ControlApiEvent),
    ChartEvent(RuntimeChartStreamEvent),
    EventStreamStatus(StreamStatus),
    ChartStreamStatus(StreamStatus),
}

#[derive(Clone, Debug)]
pub enum StreamStatus {
    Connecting,
    Open,
    Closed,
    Error(String),
}

#[allow(dead_code)]
fn fingerprint(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

struct StreamTask {
    fingerprint: u64,
    handle: JoinHandle<()>,
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn enter() -> ConsoleResult<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}
