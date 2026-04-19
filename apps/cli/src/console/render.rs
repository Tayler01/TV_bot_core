use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, Paragraph, Wrap,
    },
    Frame,
};

use super::state::{ActivityLevel, ConsoleState};

pub fn render_console(frame: &mut Frame, state: &ConsoleState) {
    let area = frame.area();
    let compact = area.width < 110;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 12 } else { 9 }),
            Constraint::Min(7),
            Constraint::Min(9),
            Constraint::Length(if compact { 3 } else { 2 }),
        ])
        .split(area);

    let summary_chunks = Layout::default()
        .direction(if compact {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(if compact {
            vec![Constraint::Length(6), Constraint::Length(6)]
        } else {
            vec![Constraint::Percentage(50), Constraint::Percentage(50)]
        })
        .split(chunks[0]);

    let mode_color = state.mode_color();
    let summary_title = Line::from(vec![
        Span::styled(
            "TV Bot Console ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[{}]", state.mode_label().to_ascii_uppercase()),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
    ]);

    let left_summary = Paragraph::new(state.left_summary_lines())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(summary_title)
                .border_style(Style::default().fg(mode_color)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(left_summary, summary_chunks[0]);

    let right_summary = Paragraph::new(state.right_summary_lines())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Runtime Posture"),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(right_summary, summary_chunks[1]);

    let chart_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if compact { 10 } else { 8 }),
            Constraint::Min(2),
        ])
        .split(chunks[1]);

    let chart_summary = Paragraph::new(state.chart_summary_lines())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(state.chart_title())
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(chart_summary, chart_chunks[0]);

    render_price_chart(frame, state, chart_chunks[1]);

    let activity_items: Vec<ListItem> = state
        .activity_items()
        .into_iter()
        .map(|item| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("[{}] ", item.label.to_ascii_uppercase()),
                    Style::default()
                        .fg(activity_color(item.level))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    item.message,
                    Style::default().fg(activity_color(item.level)),
                ),
            ]))
        })
        .collect();
    let activity_list = List::new(activity_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(state.activity_title()),
    );
    frame.render_widget(activity_list, chunks[2]);

    let footer = Paragraph::new(Line::from(footer_spans(state))).wrap(Wrap { trim: true });
    frame.render_widget(footer, chunks[3]);

    if state.show_help_overlay() {
        render_help_dialog(frame, state);
    }

    if let Some(prompt) = state.pending_confirmation_prompt() {
        render_confirmation_dialog(frame, prompt);
    }
}

fn footer_spans(state: &ConsoleState) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::raw("p paper"),
        Span::raw(" | "),
        Span::raw("o observe"),
        Span::raw(" | "),
        Span::raw("l live"),
        Span::raw(" | "),
        Span::raw("a arm"),
        Span::raw(" | "),
        Span::raw("d disarm"),
        Span::raw(" | "),
        Span::raw("space pause/resume"),
    ];

    for timeframe in state.supported_timeframes() {
        spans.push(Span::raw(" | "));
        let hotkey = match timeframe {
            tv_bot_core_types::Timeframe::OneSecond => "1",
            tv_bot_core_types::Timeframe::OneMinute => "2",
            tv_bot_core_types::Timeframe::FiveMinute => "3",
        };
        let label = format!("{hotkey} {}", crate::commands::timeframe_label(timeframe));
        let style = if state.selected_timeframe() == Some(timeframe) {
            Style::default()
                .fg(state.mode_color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
    }

    spans.extend([
        Span::raw(" | "),
        Span::raw("r refresh"),
        Span::raw(" | "),
        Span::raw("f filter"),
        Span::raw(" | "),
        Span::raw("h help"),
        Span::raw(" | "),
        Span::raw("q quit"),
        Span::raw("  |  "),
        Span::styled(state.footer_status(), Style::default().fg(Color::DarkGray)),
    ]);

    spans
}

fn render_price_chart(frame: &mut Frame, state: &ConsoleState, area: Rect) {
    let points = state.chart_points();
    if points.len() < 2 {
        let fallback =
            Paragraph::new("Waiting for enough chart bars to draw the live contract view.")
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
                        .border_style(Style::default().fg(Color::DarkGray)),
                )
                .wrap(Wrap { trim: true });
        frame.render_widget(fallback, area);
        return;
    }

    let x_bounds = state.chart_x_bounds();
    let y_bounds = state.chart_y_bounds();
    let x_labels: Vec<Line> = state.chart_x_labels().into_iter().map(Line::from).collect();
    let y_labels: Vec<Line> = state.chart_y_labels().into_iter().map(Line::from).collect();
    let current_price = state.chart_current_price_line();
    let active_position = state.chart_active_position_line();
    let (buy_fills, sell_fills) = state.chart_fill_marker_points();

    let mut datasets = vec![Dataset::default()
        .name("close")
        .graph_type(GraphType::Line)
        .marker(symbols::Marker::Braille)
        .style(Style::default().fg(state.mode_color()))
        .data(points.as_slice())];

    if !current_price.is_empty() {
        datasets.push(
            Dataset::default()
                .name("live")
                .graph_type(GraphType::Line)
                .marker(symbols::Marker::Braille)
                .style(Style::default().fg(Color::Yellow))
                .data(current_price.as_slice()),
        );
    }

    if !active_position.is_empty() {
        datasets.push(
            Dataset::default()
                .name("position")
                .graph_type(GraphType::Line)
                .marker(symbols::Marker::Braille)
                .style(Style::default().fg(Color::Cyan))
                .data(active_position.as_slice()),
        );
    }

    if !buy_fills.is_empty() {
        datasets.push(
            Dataset::default()
                .name("buy fills")
                .graph_type(GraphType::Scatter)
                .marker(symbols::Marker::Dot)
                .style(Style::default().fg(Color::Green))
                .data(buy_fills.as_slice()),
        );
    }

    if !sell_fills.is_empty() {
        datasets.push(
            Dataset::default()
                .name("sell fills")
                .graph_type(GraphType::Scatter)
                .marker(symbols::Marker::Dot)
                .style(Style::default().fg(Color::Red))
                .data(sell_fills.as_slice()),
        );
    }

    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(x_bounds)
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds(y_bounds)
                .labels(y_labels),
        );

    frame.render_widget(chart, area);
}

fn render_confirmation_dialog(frame: &mut Frame, prompt: &str) {
    let area = centered_rect(60, 7, frame.area());
    let body = format!("{prompt}\n\ny confirm   n cancel");
    let dialog = Paragraph::new(body)
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Confirmation Required")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);
}

fn render_help_dialog(frame: &mut Frame, state: &ConsoleState) {
    let timeframe_help = if state.supported_timeframes().is_empty() {
        "1/2/3 chart frames appear when the active contract supports them.".to_owned()
    } else {
        let labels = state
            .supported_timeframes()
            .into_iter()
            .map(crate::commands::timeframe_label)
            .collect::<Vec<_>>()
            .join(", ");
        format!("1/2/3 switch chart timeframe. Active frames: {labels}.")
    };
    let body = [
        "Modes and Safety",
        "p paper  o observe  l live  a arm  d disarm  space pause/resume",
        "",
        "Charts and Feed",
        timeframe_help.as_str(),
        "f cycles activity feed: all, warnings, trades, commands, system",
        "",
        "Activity Colors",
        "green success  yellow warning  red error  gray info",
        "",
        "Dismiss",
        "h or Esc closes this help. q still exits the console.",
    ]
    .join("\n");
    let area = centered_rect(72, 13, frame.area());
    let dialog = Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Console Help")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);
}

fn activity_color(level: ActivityLevel) -> Color {
    match level {
        ActivityLevel::Info => Color::Gray,
        ActivityLevel::Success => Color::Green,
        ActivityLevel::Warning => Color::Yellow,
        ActivityLevel::Error => Color::Red,
    }
}

fn centered_rect(width_percentage: u16, height: u16, area: Rect) -> Rect {
    let popup_width = area
        .width
        .saturating_mul(width_percentage)
        .saturating_div(100)
        .max(24)
        .min(area.width.saturating_sub(2).max(1));
    let popup_height = height.min(area.height.saturating_sub(2)).max(5);
    let horizontal = area.width.saturating_sub(popup_width) / 2;
    let vertical = area.height.saturating_sub(popup_height) / 2;

    Rect::new(
        area.x + horizontal,
        area.y + vertical,
        popup_width.min(area.width),
        popup_height.min(area.height),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::{backend::TestBackend, Terminal};
    use tv_bot_control_api::{RuntimeChartBar, RuntimeChartConfigResponse, RuntimeChartSnapshot};
    use tv_bot_core_types::Timeframe;

    use super::*;

    #[test]
    fn renders_on_narrow_terminal() {
        let backend = TestBackend::new(72, 22);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = ConsoleState::new(Duration::from_secs(5));

        terminal
            .draw(|frame| render_console(frame, &state))
            .unwrap();

        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("TV Bot Console"));
        assert!(screen.contains("Recent Activity"));
    }

    #[test]
    fn help_overlay_renders_on_top_of_console() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.toggle_help_overlay();

        terminal
            .draw(|frame| render_console(frame, &state))
            .unwrap();

        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("Console Help"));
        assert!(screen.contains("Activity Colors"));
    }

    #[test]
    fn line_chart_renders_loaded_series_view() {
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ConsoleState::new(Duration::from_secs(5));
        state.set_chart_snapshot_for_test(RuntimeChartSnapshot {
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
                RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.00".parse().unwrap(),
                    high: "10.10".parse().unwrap(),
                    low: "9.95".parse().unwrap(),
                    close: "10.05".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:30:00Z".parse().unwrap(),
                    is_complete: true,
                },
                RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.05".parse().unwrap(),
                    high: "10.25".parse().unwrap(),
                    low: "10.00".parse().unwrap(),
                    close: "10.20".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:31:00Z".parse().unwrap(),
                    is_complete: true,
                },
                RuntimeChartBar {
                    timeframe: Timeframe::OneMinute,
                    open: "10.20".parse().unwrap(),
                    high: "10.22".parse().unwrap(),
                    low: "10.02".parse().unwrap(),
                    close: "10.08".parse().unwrap(),
                    volume: 1,
                    closed_at: "2026-04-18T09:32:00Z".parse().unwrap(),
                    is_complete: true,
                },
            ],
            latest_price: Some("10.08".parse().unwrap()),
            latest_closed_at: Some("2026-04-18T09:32:00Z".parse().unwrap()),
            active_position: None,
            working_orders: vec![],
            recent_fills: vec![],
            can_load_older_history: false,
        });

        terminal
            .draw(|frame| render_console(frame, &state))
            .unwrap();

        let screen = buffer_text(terminal.backend().buffer());
        assert!(screen.contains("Chart Stage [1m]"));
        assert!(!screen.contains("Waiting for chart data"));
        assert!(screen.contains("TV Bot Console"));
        assert!(screen.contains("Recent Activity"));
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }
}
