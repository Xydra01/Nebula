//! UI layout and rendering for the Nebula TUI.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap,
};

use super::app::{ActivePane, TuiApp};
use nebula_proto::ModelState;

/// Renders the complete TUI onto the frame.
pub fn render(app: &TuiApp, frame: &mut Frame) {
    let size = frame.area();

    // Top-level vertical layout: Header, Resource Bar, Main Body, Footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(3), // Resource Bar
            Constraint::Min(8),    // Main Body
            Constraint::Length(1), // Footer Shortcuts
        ])
        .split(size);

    render_header(app, frame, chunks[0]);
    render_resources(app, frame, chunks[1]);
    render_main_body(app, frame, chunks[2]);
    render_footer(app, frame, chunks[3]);

    // Modal overlay on top of everything when open
    if app.approval_modal_open {
        render_approval_modal(app, frame, size);
    }
}

fn render_header(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let version = app
        .daemon_status
        .as_ref()
        .map(|s| s.version.as_str())
        .unwrap_or(env!("CARGO_PKG_VERSION"));

    let uptime = app
        .daemon_status
        .as_ref()
        .map(|s| {
            let u = s.uptime_s;
            format!("{}m {}s", u / 60, u % 60)
        })
        .unwrap_or_else(|| "--".into());

    let (model_state_str, model_state_color) = if let Some(ds) = &app.daemon_status {
        match ds.model.state {
            ModelState::Ready => ("Ready", Color::Green),
            ModelState::Busy => ("Busy", Color::Cyan),
            ModelState::Starting => ("Starting", Color::Yellow),
            ModelState::Stopped => ("Stopped", Color::DarkGray),
            ModelState::Unloaded => ("Unloaded", Color::DarkGray),
            ModelState::Restarting => ("Restarting", Color::Magenta),
            ModelState::Failed => ("Failed", Color::Red),
        }
    } else {
        ("Offline", Color::Red)
    };

    let profile = app
        .daemon_status
        .as_ref()
        .map(|s| s.model.profile.as_str())
        .unwrap_or("--");

    let gaming_str = match &app.game_mode {
        Some(gm) if gm.active => " [GAME MODE ACTIVE]",
        _ => "",
    };

    let title_line = Line::from(vec![
        Span::styled(
            " Nebula ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{version}"), Style::default().fg(Color::White)),
        Span::raw(" | Uptime: "),
        Span::styled(uptime, Style::default().fg(Color::Yellow)),
        Span::raw(" | Model: "),
        Span::styled(profile, Style::default().fg(Color::Cyan)),
        Span::raw(" ("),
        Span::styled(
            model_state_str,
            Style::default()
                .fg(model_state_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(")"),
        Span::styled(
            gaming_str,
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let header = Paragraph::new(title_line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Status ")
            .style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(header, area);
}

fn render_resources(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let (vram_str, vram_color) = if let Some(r) = &app.resources {
        let pct = if r.vram_total_mib > 0 {
            (r.vram_used_mib as f64 / r.vram_total_mib as f64) * 100.0
        } else {
            0.0
        };
        let s = format!(
            "{:.1}/{:.1} GB ({:.0}%)",
            r.vram_used_mib as f64 / 1024.0,
            r.vram_total_mib as f64 / 1024.0,
            pct
        );
        let color = if pct > 90.0 {
            Color::Red
        } else if pct > 75.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        (s, color)
    } else {
        ("N/A".into(), Color::DarkGray)
    };

    let (commit_str, commit_color) = if let Some(r) = &app.resources {
        let pct = if r.commit_limit_mib > 0 {
            (r.commit_used_mib as f64 / r.commit_limit_mib as f64) * 100.0
        } else {
            0.0
        };
        let s = format!(
            "{:.1}/{:.1} GB ({:.0}%)",
            r.commit_used_mib as f64 / 1024.0,
            r.commit_limit_mib as f64 / 1024.0,
            pct
        );
        let color = if pct > 85.0 {
            Color::Red
        } else if pct > 70.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        (s, color)
    } else {
        ("N/A".into(), Color::DarkGray)
    };

    let gpu_util_str = if let Some(r) = &app.resources {
        r.gpu_util_pct
            .map(|u| format!("{u}%"))
            .unwrap_or_else(|| "--".into())
    } else {
        "N/A".into()
    };

    let cpu_util_str = if let Some(r) = &app.resources {
        format!("{:.1}%", r.cpu_pct)
    } else {
        "N/A".into()
    };

    let res_line = Line::from(vec![
        Span::raw("VRAM: "),
        Span::styled(
            vram_str,
            Style::default().fg(vram_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  Commit: "),
        Span::styled(commit_str, Style::default().fg(commit_color)),
        Span::raw("  |  GPU: "),
        Span::styled(gpu_util_str, Style::default().fg(Color::Cyan)),
        Span::raw("  |  CPU: "),
        Span::styled(cpu_util_str, Style::default().fg(Color::White)),
    ]);

    let res_block = Paragraph::new(res_line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Resources ")
            .style(Style::default().fg(Color::DarkGray)),
    );

    frame.render_widget(res_block, area);
}

fn render_main_body(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40), // Left: Tasks & Tools
            Constraint::Percentage(60), // Right: Streaming Output
        ])
        .split(area);

    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50), // Tasks
            Constraint::Percentage(50), // Tool Calls
        ])
        .split(main_chunks[0]);

    render_tasks_pane(app, frame, left_chunks[0]);
    render_tools_pane(app, frame, left_chunks[1]);
    render_streaming_pane(app, frame, main_chunks[1]);
}

fn render_tasks_pane(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Tasks;
    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let items: Vec<ListItem> = if app.tasks.is_empty() {
        vec![ListItem::new(Span::styled(
            " No tasks recorded",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let (status_badge, status_color) = match task.status {
                    crate::tui::TuiTaskStatus::Running => ("[RUNNING]", Color::Cyan),
                    crate::tui::TuiTaskStatus::Completed => ("[COMPLETED]", Color::Green),
                    crate::tui::TuiTaskStatus::Failed => ("[FAILED]", Color::Red),
                    crate::tui::TuiTaskStatus::LimitReached => ("[LIMIT]", Color::Yellow),
                    crate::tui::TuiTaskStatus::Cancelled => ("[CANCELLED]", Color::DarkGray),
                };

                let selected = i == app.selected_task;
                let prefix = if selected { "> " } else { "  " };

                let line = Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(status_badge, Style::default().fg(status_color)),
                    Span::raw(" "),
                    Span::styled(
                        format!("{:.8}..", task.id),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" (steps: {}) ", task.step_count)),
                    Span::styled(&task.summary, Style::default().fg(Color::DarkGray)),
                ]);

                let item_style = if selected && is_focused {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

                ListItem::new(line).style(item_style)
            })
            .collect()
    };

    let tasks_list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(if is_focused {
                BorderType::Thick
            } else {
                BorderType::Rounded
            })
            .border_style(Style::default().fg(border_color))
            .title(" Tasks (Tab: 1) "),
    );

    frame.render_widget(tasks_list, area);
}

fn render_tools_pane(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let is_focused = app.active_pane == ActivePane::ToolCalls;
    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let rows: Vec<Row> = if app.tool_calls.is_empty() {
        vec![Row::new(vec![Cell::from("No recent tool calls")])]
    } else {
        app.tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                let selected = i == app.selected_tool_call;
                let row_style = if selected && is_focused {
                    Style::default().bg(Color::DarkGray)
                } else {
                    Style::default()
                };

                Row::new(vec![
                    Cell::from(format!("#{}", tc.step_index)),
                    Cell::from(tc.summary.clone()),
                    Cell::from(Span::styled(&tc.status, Style::default().fg(Color::Green))),
                ])
                .style(row_style)
            })
            .collect()
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Percentage(70),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(vec!["Step", "Tool / Action", "Status"]).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(if is_focused {
                BorderType::Thick
            } else {
                BorderType::Rounded
            })
            .border_style(Style::default().fg(border_color))
            .title(" Tool Calls (Tab: 3) "),
    );

    frame.render_widget(table, area);
}

fn render_streaming_pane(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Streaming;
    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let text_content = if app.streaming_text.is_empty() {
        "Awaiting model output or task tokens..."
    } else {
        &app.streaming_text
    };

    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;

    let total_visual_lines: usize = text_content
        .split('\n')
        .map(|l| {
            let len = l.chars().count();
            if len == 0 {
                1
            } else {
                len.div_ceil(inner_width)
            }
        })
        .sum();

    let max_scroll = total_visual_lines.saturating_sub(inner_height);
    let scroll_y = if app.auto_scroll {
        max_scroll
    } else {
        app.streaming_scroll.min(max_scroll)
    };

    let scroll_status = if app.auto_scroll {
        "Auto-Scroll".to_string()
    } else {
        format!("Scroll: {scroll_y}/{total_visual_lines}")
    };

    let scroll_u16 = u16::try_from(scroll_y).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(Text::raw(text_content))
        .wrap(Wrap { trim: false })
        .scroll((scroll_u16, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(if is_focused {
                    BorderType::Thick
                } else {
                    BorderType::Rounded
                })
                .border_style(Style::default().fg(border_color))
                .title(format!(
                    " Streaming Model Output (Tab: 2) [{scroll_status}] "
                )),
        );

    frame.render_widget(paragraph, area);
}

fn render_footer(_app: &TuiApp, frame: &mut Frame, area: Rect) {
    let shortcuts = Line::from(vec![
        Span::styled(
            "[q] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Quit  "),
        Span::styled(
            "[Tab] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Cycle Pane  "),
        Span::styled(
            "[↑/↓] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Navigate/Scroll  "),
        Span::styled(
            "[r] ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Refresh  "),
        Span::styled(
            "[a] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Approve  "),
        Span::styled(
            "[d] ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("Deny  "),
    ]);

    let footer = Paragraph::new(shortcuts).alignment(Alignment::Left);
    frame.render_widget(footer, area);
}

fn render_approval_modal(app: &TuiApp, frame: &mut Frame, area: Rect) {
    let Some(req) = &app.pending_approval else {
        return;
    };

    let popup_width = 70.min(area.width.saturating_sub(4));
    let popup_height = if app.show_details { 16 } else { 12 };
    let popup_height = popup_height.min(area.height.saturating_sub(2));

    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Command: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                &req.command,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Tier:    ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                &req.tier,
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Trace:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(&req.trace_id, Style::default().fg(Color::DarkGray)),
        ]),
    ];

    if let Some(rule) = &req.rule_id {
        lines.push(Line::from(vec![
            Span::styled("Rule:    ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(rule, Style::default().fg(Color::Yellow)),
        ]));
    }

    lines.push(Line::raw(""));

    if app.show_details {
        lines.push(Line::styled(
            &req.summary,
            Style::default().fg(Color::White),
        ));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(vec![
        Span::styled(
            "  [A] Approve  ",
            Style::default()
                .bg(Color::Green)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("    "),
        Span::styled(
            "  [D] Deny  ",
            Style::default()
                .bg(Color::Red)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("    "),
        Span::styled(
            "  [V] Details  ",
            Style::default().bg(Color::Blue).fg(Color::White),
        ),
        Span::raw("    "),
        Span::styled("  [Esc] Cancel  ", Style::default().fg(Color::DarkGray)),
    ]));

    let block = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" ⚠ APPROVAL REQUIRED (Tier 3) ")
            .title_alignment(Alignment::Center),
    );

    frame.render_widget(block, popup_area);
}
