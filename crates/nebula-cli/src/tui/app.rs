//! Application state and event handling for the Nebula TUI.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nebula_proto::{
    ApprovalNeeded, DaemonStatus, Event, GameModeStatusResponse, ResourceSnapshot, TaskId,
    TaskStatus,
};

/// Maximum tool calls retained in memory for the TUI list.
const MAX_TOOL_CALLS: usize = 100;

/// Active pane with keyboard focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActivePane {
    /// Tasks list pane.
    #[default]
    Tasks,
    /// Live streaming output pane.
    Streaming,
    /// Tool calls list pane.
    ToolCalls,
}

impl ActivePane {
    /// Cycles to the next pane.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Tasks => Self::Streaming,
            Self::Streaming => Self::ToolCalls,
            Self::ToolCalls => Self::Tasks,
        }
    }

    /// Cycles to the previous pane.
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Tasks => Self::ToolCalls,
            Self::Streaming => Self::Tasks,
            Self::ToolCalls => Self::Streaming,
        }
    }
}

/// Current execution or completion status of a task for the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiTaskStatus {
    /// In progress.
    Running,
    /// Successfully completed.
    Completed,
    /// Reached budget or limit.
    LimitReached,
    /// Cancelled.
    Cancelled,
    /// Failed with error.
    Failed,
}

impl From<TaskStatus> for TuiTaskStatus {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Completed => Self::Completed,
            TaskStatus::LimitReached => Self::LimitReached,
            TaskStatus::Cancelled => Self::Cancelled,
            TaskStatus::Failed => Self::Failed,
        }
    }
}

/// A task item displayed in the tasks list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskItem {
    /// Task identifier.
    pub id: TaskId,
    /// Current task status.
    pub status: TuiTaskStatus,
    /// Short summary or goal.
    pub summary: String,
    /// Number of tool steps completed.
    pub step_count: usize,
}

/// A recorded tool call step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallItem {
    /// Task ID.
    pub task_id: TaskId,
    /// Step index.
    pub step_index: u32,
    /// Tool name and argument summary.
    pub summary: String,
    /// Status or result.
    pub status: String,
}

/// Action to dispatch from the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiAction {
    /// Send an approval response (true = approved, false = denied).
    ApprovalResponse {
        /// Approval request being answered.
        request: ApprovalNeeded,
        /// Approved flag.
        approved: bool,
    },
    /// Refresh resource snapshot.
    RefreshResources,
    /// Quit application.
    Quit,
}

/// Application state for the Nebula TUI.
#[derive(Debug, Default)]
pub struct TuiApp {
    /// Recent tasks.
    pub tasks: Vec<TaskItem>,
    /// Selected index in tasks list.
    pub selected_task: usize,
    /// Streaming model output text.
    pub streaming_text: String,
    /// Vertical scroll offset for streaming text.
    pub streaming_scroll: usize,
    /// Whether streaming text auto-scrolls to the end on new tokens.
    pub auto_scroll: bool,
    /// Recent tool calls.
    pub tool_calls: VecDeque<ToolCallItem>,
    /// Selected index in tool calls.
    pub selected_tool_call: usize,
    /// Latest resource snapshot.
    pub resources: Option<ResourceSnapshot>,
    /// Latest daemon status.
    pub daemon_status: Option<DaemonStatus>,
    /// Active game mode status.
    pub game_mode: Option<GameModeStatusResponse>,
    /// Pending approval modal, if any.
    pub pending_approval: Option<ApprovalNeeded>,
    /// Whether approval modal is open.
    pub approval_modal_open: bool,
    /// Whether details modal is open.
    pub show_details: bool,
    /// Currently focused pane.
    pub active_pane: ActivePane,
    /// Whether application should exit.
    pub should_quit: bool,
    /// Pending actions to dispatch to the daemon.
    pub pending_actions: VecDeque<TuiAction>,
}

impl TuiApp {
    /// Creates a fresh TUI state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            auto_scroll: true,
            ..Self::default()
        }
    }

    /// Handles keyboard input.
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Modal mode intercepts keys first
        if self.approval_modal_open {
            match key.code {
                KeyCode::Char('a' | 'A') => {
                    if let Some(req) = self.pending_approval.take() {
                        self.pending_actions.push_back(TuiAction::ApprovalResponse {
                            request: req,
                            approved: true,
                        });
                    }
                    self.approval_modal_open = false;
                    self.show_details = false;
                }
                KeyCode::Char('d' | 'D') | KeyCode::Esc => {
                    if let Some(req) = self.pending_approval.take() {
                        self.pending_actions.push_back(TuiAction::ApprovalResponse {
                            request: req,
                            approved: false,
                        });
                    }
                    self.approval_modal_open = false;
                    self.show_details = false;
                }
                KeyCode::Char('v' | 'V') => {
                    self.show_details = !self.show_details;
                }
                _ => {}
            }
            return;
        }

        // Global keybindings
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), _) => {
                self.should_quit = true;
                self.pending_actions.push_back(TuiAction::Quit);
            }
            (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::BackTab, _) => {
                self.active_pane = self.active_pane.prev();
            }
            (KeyCode::Tab, _) => {
                self.active_pane = self.active_pane.next();
            }
            (KeyCode::Char('1'), _) => {
                self.active_pane = ActivePane::Tasks;
            }
            (KeyCode::Char('2'), _) => {
                self.active_pane = ActivePane::Streaming;
            }
            (KeyCode::Char('3'), _) => {
                self.active_pane = ActivePane::ToolCalls;
            }
            (KeyCode::Char('r'), _) => {
                self.pending_actions.push_back(TuiAction::RefreshResources);
            }
            // Pane-specific navigation
            (KeyCode::Up | KeyCode::Char('k'), _) => match self.active_pane {
                ActivePane::Tasks => {
                    if self.selected_task > 0 {
                        self.selected_task -= 1;
                    }
                }
                ActivePane::Streaming => {
                    if self.auto_scroll {
                        let lines = self.streaming_text.lines().count();
                        self.streaming_scroll = lines;
                        self.auto_scroll = false;
                    }
                    if self.streaming_scroll > 0 {
                        self.streaming_scroll -= 1;
                    }
                }
                ActivePane::ToolCalls => {
                    if self.selected_tool_call > 0 {
                        self.selected_tool_call -= 1;
                    }
                }
            },
            (KeyCode::Down | KeyCode::Char('j'), _) => match self.active_pane {
                ActivePane::Tasks => {
                    if !self.tasks.is_empty() && self.selected_task + 1 < self.tasks.len() {
                        self.selected_task += 1;
                    }
                }
                ActivePane::Streaming => {
                    self.streaming_scroll += 1;
                    let lines = self.streaming_text.lines().count();
                    if self.streaming_scroll >= lines {
                        self.auto_scroll = true;
                    }
                }
                ActivePane::ToolCalls => {
                    if !self.tool_calls.is_empty()
                        && self.selected_tool_call + 1 < self.tool_calls.len()
                    {
                        self.selected_tool_call += 1;
                    }
                }
            },
            (KeyCode::PageUp, _) if self.active_pane == ActivePane::Streaming => {
                if self.auto_scroll {
                    let lines = self.streaming_text.lines().count();
                    self.streaming_scroll = lines;
                    self.auto_scroll = false;
                }
                self.streaming_scroll = self.streaming_scroll.saturating_sub(10);
            }
            (KeyCode::PageDown, _) if self.active_pane == ActivePane::Streaming => {
                self.streaming_scroll = self.streaming_scroll.saturating_add(10);
                let lines = self.streaming_text.lines().count();
                if self.streaming_scroll >= lines {
                    self.auto_scroll = true;
                }
            }
            (KeyCode::Home, _) if self.active_pane == ActivePane::Streaming => {
                self.streaming_scroll = 0;
                self.auto_scroll = false;
            }
            (KeyCode::End, _) if self.active_pane == ActivePane::Streaming => {
                let lines = self.streaming_text.lines().count();
                self.streaming_scroll = lines;
                self.auto_scroll = true;
            }
            _ => {}
        }
    }

    /// Handles events arriving from the daemon pipe.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::ChatToken(t) => {
                if !t.reasoning.is_empty() {
                    self.streaming_text.push_str(&t.reasoning);
                }
                if !t.text.is_empty() {
                    if t.text.contains("[ERROR]")
                        && let Some(last_tc) = self.tool_calls.back_mut()
                    {
                        last_tc.status = "error".into();
                    } else if t.text.contains("[OK]")
                        && let Some(last_tc) = self.tool_calls.back_mut()
                    {
                        last_tc.status = "ok".into();
                    }
                    self.streaming_text.push_str(&t.text);
                }
                if self.auto_scroll {
                    self.streaming_scroll = self.streaming_text.lines().count();
                }
            }
            Event::TaskStep(step) => {
                // Update task or insert
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == step.task_id) {
                    task.step_count = step.step_index as usize;
                } else {
                    self.tasks.push(TaskItem {
                        id: step.task_id,
                        status: TuiTaskStatus::Running,
                        summary: format!("Task {}", step.task_id),
                        step_count: step.step_index as usize,
                    });
                }

                // Record tool calls with initial 'running' status
                for call in &step.tool_calls {
                    self.tool_calls.push_back(ToolCallItem {
                        task_id: step.task_id,
                        step_index: step.step_index,
                        summary: call.clone(),
                        status: "running".into(),
                    });
                    if self.tool_calls.len() > MAX_TOOL_CALLS {
                        self.tool_calls.pop_front();
                    }
                }

                let tools_desc = if step.tool_calls.is_empty() {
                    "thinking".to_string()
                } else {
                    step.tool_calls.join(", ")
                };
                let prefix =
                    if self.streaming_text.is_empty() || self.streaming_text.ends_with('\n') {
                        ""
                    } else {
                        "\n"
                    };
                self.streaming_text.push_str(&format!(
                    "{prefix}─── Step {} [{}] ───\n",
                    step.step_index, tools_desc
                ));
                if self.auto_scroll {
                    self.streaming_scroll = self.streaming_text.lines().count();
                }
            }
            Event::TaskDone(done) => {
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == done.task_id) {
                    task.status = done.status.into();
                    task.summary = done.summary.clone();
                } else {
                    self.tasks.push(TaskItem {
                        id: done.task_id,
                        status: done.status.into(),
                        summary: done.summary.clone(),
                        step_count: 0,
                    });
                }

                let prefix =
                    if self.streaming_text.is_empty() || self.streaming_text.ends_with('\n') {
                        ""
                    } else {
                        "\n"
                    };
                self.streaming_text.push_str(&format!(
                    "{prefix}═══ Task Done [{:?}] ═══\n{}\n",
                    done.status, done.summary
                ));
                if self.auto_scroll {
                    self.streaming_scroll = self.streaming_text.lines().count();
                }
            }
            Event::ApprovalNeeded(req) => {
                let prefix =
                    if self.streaming_text.is_empty() || self.streaming_text.ends_with('\n') {
                        ""
                    } else {
                        "\n"
                    };
                self.streaming_text.push_str(&format!(
                    "{prefix}⚠ APPROVAL NEEDED: command '{}' ({})\n",
                    req.command, req.tier
                ));
                if self.auto_scroll {
                    self.streaming_scroll = self.streaming_text.lines().count();
                }
                self.pending_approval = Some(req);
                self.approval_modal_open = true;
            }
            Event::ModelStateChanged(ch) => {
                if let Some(ds) = &mut self.daemon_status {
                    ds.model.state = ch.to;
                }
            }
            Event::GameModeChanged(gm) => {
                self.game_mode = Some(GameModeStatusResponse {
                    active: gm.active,
                    manual_override: None,
                    reason: gm.reason,
                    process_name: None,
                });
            }
            _ => {}
        }
    }

    /// Updates resource snapshot.
    pub fn update_resources(&mut self, snap: ResourceSnapshot) {
        self.resources = Some(snap);
    }

    /// Updates daemon status.
    pub fn update_daemon_status(&mut self, status: DaemonStatus) {
        self.daemon_status = Some(status);
    }
}
