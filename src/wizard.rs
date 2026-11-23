//! Interactive setup: course path → AI backend → layout → plan preview.

use crate::ai::{self, AiConfig, Backend};
use crate::layout::{self, Kind, LayoutMode, Plan};
use anyhow::Result;
use ratatui::crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const ACCENT: Color = Color::Rgb(94, 234, 212);
const ACCENT2: Color = Color::Rgb(196, 148, 255);
const WARM: Color = Color::Rgb(255, 196, 112);
const OK: Color = Color::Rgb(134, 239, 172);
const BAD: Color = Color::Rgb(252, 129, 129);
const DIM: Color = Color::Rgb(110, 118, 135);
const FG: Color = Color::Rgb(220, 224, 232);
const BORDER: Color = Color::Rgb(60, 66, 82);
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct Choice {
    pub ai: AiConfig,
    pub plan: Plan,
}

#[derive(PartialEq)]
enum Step {
    Path,
    Ai,
    Model,
    Layout,
    Planning,
    Preview,
}

struct Wizard {
    step: Step,
    out: PathBuf,
    path: String,
    error: Option<String>,
    found: Option<(PathBuf, Vec<String>)>,
    backends: Vec<(Backend, bool, String)>,
    ai_state: ListState,
    model: String,
    layouts: Vec<(LayoutMode, &'static str, &'static str)>,
    layout_state: ListState,
    planning: Option<Receiver<Result<Plan, String>>>,
    plan: Option<Plan>,
    preview_state: ListState,
    tick: usize,
}

/// Runs the wizard. `None` means the user quit.
pub fn run(initial: Option<PathBuf>, out: &Path, ai: Option<AiConfig>) -> Result<Option<Choice>> {
    let mut terminal = ratatui::init();
    let mut w = Wizard {
        step: Step::Path,
        out: out.to_path_buf(),
        path: initial
            .map(|p| p.display().to_string())
            .unwrap_or_else(default_path),
        error: None,
        found: None,
        backends: Vec::new(),
        ai_state: ListState::default().with_selected(Some(0)),
        model: ai
            .as_ref()
            .and_then(|a| a.model.clone())
            .unwrap_or_default(),
        layouts: vec![
            (
                LayoutMode::Auto,
                "Use folders",
                "one unit per folder that holds videos, at any depth (or a saved plan.json)",
            ),
            (
                LayoutMode::Ai,
                "Ask the AI",
                "the chosen AI reads the file tree and groups lessons into units",
            ),
            (
                LayoutMode::Folders,
                "Folders, fresh",
                "ignore any saved plan.json and regroup by folders",
            ),
        ],
        layout_state: ListState::default().with_selected(Some(0)),
        planning: None,
        plan: None,
        preview_state: ListState::default().with_selected(Some(0)),
        tick: 0,
    };
    let preselect = ai.map(|a| a.backend);
    let result = w.event_loop(&mut terminal, preselect);
    ratatui::restore();
    result
}

fn default_path() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let course = cwd.join("course");
    if course.is_dir() {
        course.display().to_string()
    } else {
        cwd.display().to_string()
    }
}

fn expand(p: &str) -> PathBuf {
    let p = p.trim().trim_matches(['"', '\'']);
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest),
        None => PathBuf::from(p.replace("\\ ", " ")),
    }
}

impl Wizard {
    fn event_loop(
        &mut self,
        terminal: &mut DefaultTerminal,
        preselect: Option<Backend>,
    ) -> Result<Option<Choice>> {
        loop {
            self.tick = self.tick.wrapping_add(1);
            if let Some(rx) = &self.planning
                && let Ok(res) = rx.try_recv()
            {
                self.planning = None;
                match res {
                    Ok(plan) => {
                        self.plan = Some(plan);
                        self.step = Step::Preview;
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.step = Step::Layout;
                    }
                }
            }
            terminal.draw(|f| self.draw(f))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let TermEvent::Key(k) = event::read()? else {
                continue;
            };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(None);
            }
            if let Some(done) = self.key(k, preselect)? {
                return Ok(done);
            }
        }
    }

    /// Returns `Some(result)` when the wizard should close.
    fn key(&mut self, k: KeyEvent, preselect: Option<Backend>) -> Result<Option<Option<Choice>>> {
        match self.step {
            Step::Path => match k.code {
                KeyCode::Esc => return Ok(Some(None)),
                KeyCode::Char(c) => self.path.push(c),
                KeyCode::Backspace => {
                    self.path.pop();
                }
                KeyCode::Enter => {
                    let out = self.out.canonicalize().unwrap_or_else(|_| self.out.clone());
                    match layout::inventory(&expand(&self.path), &[out]) {
                        Ok(found) => {
                            self.found = Some(found);
                            self.error = None;
                            self.load_backends(preselect);
                            self.step = Step::Ai;
                        }
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                _ => {}
            },
            Step::Ai => match k.code {
                KeyCode::Esc => self.step = Step::Path,
                KeyCode::Up | KeyCode::Char('k') => self.ai_state.select_previous(),
                KeyCode::Down | KeyCode::Char('j') => self.ai_state.select_next(),
                KeyCode::Char('m') => self.step = Step::Model,
                KeyCode::Enter => {
                    let i = self
                        .ai_state
                        .selected()
                        .unwrap_or(0)
                        .min(self.backends.len() - 1);
                    let (b, ok, why) = &self.backends[i];
                    if *ok {
                        self.error = None;
                        self.step = Step::Layout;
                    } else {
                        self.error = Some(format!("{} is not ready: {why}", b.label()));
                    }
                }
                _ => {}
            },
            Step::Model => match k.code {
                KeyCode::Esc | KeyCode::Enter => self.step = Step::Ai,
                KeyCode::Char(c) => self.model.push(c),
                KeyCode::Backspace => {
                    self.model.pop();
                }
                _ => {}
            },
            Step::Layout => match k.code {
                KeyCode::Esc => self.step = Step::Ai,
                KeyCode::Up | KeyCode::Char('k') => self.layout_state.select_previous(),
                KeyCode::Down | KeyCode::Char('j') => self.layout_state.select_next(),
                KeyCode::Enter => self.start_planning(),
                _ => {}
            },
            Step::Planning => {
                if k.code == KeyCode::Esc {
                    self.planning = None;
                    self.step = Step::Layout;
                }
            }
            Step::Preview => match k.code {
                KeyCode::Esc | KeyCode::Char('b') => self.step = Step::Layout,
                KeyCode::Up | KeyCode::Char('k') => self.preview_state.select_previous(),
                KeyCode::Down | KeyCode::Char('j') => self.preview_state.select_next(),
                KeyCode::Char('q') => return Ok(Some(None)),
                KeyCode::Enter => {
                    if let Some(plan) = self.plan.take() {
                        return Ok(Some(Some(Choice {
                            ai: self.ai_config(),
                            plan,
                        })));
                    }
                }
                _ => {}
            },
        }
        Ok(None)
    }

    fn load_backends(&mut self, preselect: Option<Backend>) {
        if self.backends.is_empty() {
            self.backends = ai::ALL
                .iter()
                .map(|b| {
                    let (ok, why) = b.availability();
                    (*b, ok, why)
                })
                .collect();
        }
        let pick = preselect
            .and_then(|p| self.backends.iter().position(|(b, _, _)| *b == p))
            .or_else(|| self.backends.iter().position(|(_, ok, _)| *ok))
            .unwrap_or(0);
        self.ai_state.select(Some(pick));
    }

    fn ai_config(&self) -> AiConfig {
        let i = self
            .ai_state
            .selected()
            .unwrap_or(0)
            .min(self.backends.len().saturating_sub(1));
        AiConfig {
            backend: self.backends.get(i).map_or(Backend::Claude, |b| b.0),
            model: Some(self.model.trim().to_string()).filter(|m| !m.is_empty()),
            base_url: None,
        }
    }

    fn start_planning(&mut self) {
        let Some((root, files)) = self.found.clone() else {
            return;
        };
        let mode = self.layouts[self
            .layout_state
            .selected()
            .unwrap_or(0)
            .min(self.layouts.len() - 1)]
        .0;
        let ai = self.ai_config();
        let out = self.out.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ =
                tx.send(make_plan(&root, &files, mode, &ai, &out).map_err(|e| format!("{e:#}")));
        });
        self.planning = Some(rx);
        self.error = None;
        self.step = Step::Planning;
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .areas(f.area());
        let steps = ["Course", "AI", "Layout", "Review"];
        let current = match self.step {
            Step::Path => 0,
            Step::Ai | Step::Model => 1,
            Step::Layout | Step::Planning => 2,
            Step::Preview => 3,
        };
        let mut spans = vec![
            Span::styled(" course", Style::default().fg(ACCENT).bold()),
            Span::styled("2", Style::default().fg(WARM).bold()),
            Span::styled("skill   ", Style::default().fg(ACCENT2).bold()),
        ];
        for (i, s) in steps.iter().enumerate() {
            let style = match i.cmp(&current) {
                std::cmp::Ordering::Less => Style::default().fg(OK),
                std::cmp::Ordering::Equal => Style::default().fg(WARM).bold(),
                std::cmp::Ordering::Greater => Style::default().fg(DIM),
            };
            let mark = if i < current { "✔" } else { "●" };
            spans.push(Span::styled(format!("{mark} {} {s}", i + 1), style));
            if i + 1 < steps.len() {
                spans.push(Span::styled("  ─  ", Style::default().fg(BORDER)));
            }
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).block(frame_block("")),
            header,
        );

        match self.step {
            Step::Path => self.draw_path(f, body),
            Step::Ai | Step::Model => self.draw_ai(f, body),
            Step::Layout => self.draw_layout(f, body),
            Step::Planning => {
                let msg = format!("{} Planning units…", SPINNER[self.tick % SPINNER.len()]);
                f.render_widget(
                    Paragraph::new(Span::styled(msg, Style::default().fg(WARM)))
                        .block(frame_block("Layout")),
                    body,
                );
            }
            Step::Preview => self.draw_preview(f, body),
        }

        let hint = match self.step {
            Step::Path => "type a path · enter continue · esc quit",
            Step::Ai => "↑↓ choose · m set model · enter continue · esc back",
            Step::Model => "type a model name (blank = default) · enter done",
            Step::Layout => "↑↓ choose · enter plan · esc back",
            Step::Planning => "esc cancel",
            Step::Preview => "enter start · ↑↓ scroll · b back · q quit",
        };
        let mut lines = vec![Line::from(Span::styled(
            format!(" {hint}"),
            Style::default().fg(DIM),
        ))];
        if let Some(e) = &self.error {
            lines.insert(
                0,
                Line::from(Span::styled(format!(" ✘ {e}"), Style::default().fg(BAD))),
            );
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), footer);
    }

    fn draw_path(&self, f: &mut Frame, area: Rect) {
        let cursor = if (self.tick / 5).is_multiple_of(2) {
            "▏"
        } else {
            " "
        };
        let lines = vec![
            Line::from(Span::styled(
                "Where is the course?",
                Style::default().fg(FG).bold(),
            )),
            Line::from(Span::styled(
                "Any layout works: chapter folders, nested modules, or one flat folder of videos.",
                Style::default().fg(DIM),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled(" › ", Style::default().fg(ACCENT).bold()),
                Span::styled(self.path.clone(), Style::default().fg(FG)),
                Span::styled(cursor, Style::default().fg(ACCENT)),
            ]),
        ];
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(frame_block("Course")),
            area,
        );
    }

    fn draw_ai(&mut self, f: &mut Frame, area: Rect) {
        let [list_area, info] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(4)]).areas(area);
        let items: Vec<ListItem> = self
            .backends
            .iter()
            .map(|(b, ok, why)| {
                let (mark, color) = if *ok { ("●", OK) } else { ("○", DIM) };
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(format!("{mark} "), Style::default().fg(color)),
                        Span::styled(format!("{:<22}", b.label()), Style::default().fg(FG).bold()),
                        Span::styled(why.clone(), Style::default().fg(color)),
                    ]),
                    Line::from(Span::styled(
                        format!("    {}", b.hint()),
                        Style::default().fg(DIM),
                    )),
                ])
            })
            .collect();
        let list = List::new(items)
            .block(frame_block("Which AI writes the skills?"))
            .highlight_style(Style::default().bg(Color::Rgb(36, 40, 52)))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, list_area, &mut self.ai_state);
        let editing = self.step == Step::Model;
        let model = if self.model.is_empty() && !editing {
            "default".to_string()
        } else {
            self.model.clone()
        };
        let lines = vec![
            Line::from(vec![
                Span::styled(" model: ", Style::default().fg(DIM)),
                Span::styled(
                    model,
                    Style::default().fg(if editing { WARM } else { FG }).bold(),
                ),
                Span::styled(if editing { "▏" } else { "" }, Style::default().fg(WARM)),
            ]),
            Line::from(Span::styled(
                " Transcription always runs locally (Qwen3-ASR on MLX). This AI only writes the skills.",
                Style::default().fg(DIM),
            )),
        ];
        f.render_widget(Paragraph::new(lines).block(frame_block("")), info);
    }

    fn draw_layout(&mut self, f: &mut Frame, area: Rect) {
        let [summary, list_area] =
            Layout::vertical([Constraint::Length(4), Constraint::Min(5)]).areas(area);
        if let Some((root, files)) = &self.found {
            let videos = files
                .iter()
                .filter(|f| layout::kind(Path::new(f)) == Some(Kind::Video))
                .count();
            let folders: std::collections::BTreeSet<_> =
                files.iter().filter_map(|f| Path::new(f).parent()).collect();
            let lines = vec![
                Line::from(vec![
                    Span::styled(" Found ", Style::default().fg(DIM)),
                    Span::styled(root.display().to_string(), Style::default().fg(FG).bold()),
                ]),
                Line::from(Span::styled(
                    format!(
                        " {videos} videos · {} handouts · {} folders",
                        files.len() - videos,
                        folders.len()
                    ),
                    Style::default().fg(ACCENT),
                )),
            ];
            f.render_widget(Paragraph::new(lines).block(frame_block("")), summary);
        }
        let items: Vec<ListItem> = self
            .layouts
            .iter()
            .map(|(_, name, hint)| {
                ListItem::new(vec![
                    Line::from(Span::styled(*name, Style::default().fg(FG).bold())),
                    Line::from(Span::styled(format!("  {hint}"), Style::default().fg(DIM))),
                ])
            })
            .collect();
        let list = List::new(items)
            .block(frame_block("How should lessons be grouped into skills?"))
            .highlight_style(Style::default().bg(Color::Rgb(36, 40, 52)))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, list_area, &mut self.layout_state);
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let Some(plan) = &self.plan else { return };
        let total: usize = plan.units.iter().map(|u| u.files.len()).sum();
        let items: Vec<ListItem> = plan
            .units
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let videos = u
                    .files
                    .iter()
                    .filter(|f| layout::kind(Path::new(f)) == Some(Kind::Video))
                    .count();
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(format!("{:>2}  ", i + 1), Style::default().fg(DIM)),
                        Span::styled(u.name.clone(), Style::default().fg(FG).bold()),
                    ]),
                    Line::from(Span::styled(
                        format!(
                            "    {videos} videos · {} handouts → result/{}",
                            u.files.len() - videos,
                            u.folder
                        ),
                        Style::default().fg(DIM),
                    )),
                ])
            })
            .collect();
        let title = format!(
            "{} units from {total} files · plan saved to {}/plan.json",
            plan.units.len(),
            self.out.display()
        );
        let list = List::new(items)
            .block(frame_block(&title))
            .highlight_style(Style::default().bg(Color::Rgb(36, 40, 52)))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, area, &mut self.preview_state);
    }
}

fn frame_block(title: &str) -> Block<'_> {
    let b = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .padding(Padding::horizontal(1));
    if title.is_empty() {
        b
    } else {
        b.title(Span::styled(
            format!(" {title} "),
            Style::default().fg(ACCENT).bold(),
        ))
    }
}

/// Builds and saves the plan for a course root (shared by the wizard and the CLI).
pub fn make_plan(
    root: &Path,
    files: &[String],
    mode: LayoutMode,
    ai_cfg: &AiConfig,
    out: &Path,
) -> Result<Plan> {
    let course = layout::file_name(root);
    if mode == LayoutMode::Auto
        && let Some(saved) = layout::load_plan(out, root)
    {
        return Ok(saved);
    }
    let plan = match mode {
        LayoutMode::Ai => {
            let mut durations = BTreeMap::new();
            for f in files
                .iter()
                .filter(|f| layout::kind(Path::new(f)) == Some(Kind::Video))
            {
                if let Ok(d) = crate::media::probe_duration(&root.join(f)) {
                    durations.insert(f.clone(), d);
                }
            }
            layout::ai_plan(&course, root, files, &durations, ai_cfg)?
        }
        _ => layout::heuristic_plan(&course, root, files),
    };
    layout::save_plan(&plan, out)?;
    Ok(plan)
}
