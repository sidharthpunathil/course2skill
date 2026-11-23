//! ratatui front end. Consumes pipeline events and draws the dashboard.

use crate::pipeline::{ChInfo, ChState, Event};
use crate::transcript::fmt_ts;
use anyhow::Result;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Gauge, LineGauge, List, ListItem, Padding, Paragraph, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const ACCENT: Color = Color::Rgb(94, 234, 212);
const ACCENT2: Color = Color::Rgb(196, 148, 255);
const WARM: Color = Color::Rgb(255, 196, 112);
const OK: Color = Color::Rgb(134, 239, 172);
const BAD: Color = Color::Rgb(252, 129, 129);
const DIM: Color = Color::Rgb(110, 118, 135);
const FG: Color = Color::Rgb(220, 224, 232);
const BORDER: Color = Color::Rgb(60, 66, 82);
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

struct App {
    course: String,
    stage: String,
    chapters: Vec<ChInfo>,
    states: Vec<ChState>,
    current_chapter: Option<usize>,
    video: String,
    video_duration: f64,
    chunk: (usize, usize),
    total_audio: f64,
    done_audio: f64,
    fresh_audio: f64,
    transcript: VecDeque<(f64, String)>,
    logs: VecDeque<(bool, String)>,
    started: Instant,
    asr_started: Option<Instant>,
    finished: Option<(bool, String)>,
    tick: usize,
}

impl App {
    fn new() -> App {
        App {
            course: String::from("scanning…"),
            stage: String::from("Starting"),
            chapters: Vec::new(),
            states: Vec::new(),
            current_chapter: None,
            video: String::new(),
            video_duration: 0.0,
            chunk: (0, 0),
            total_audio: 0.0,
            done_audio: 0.0,
            fresh_audio: 0.0,
            transcript: VecDeque::new(),
            logs: VecDeque::new(),
            started: Instant::now(),
            asr_started: None,
            finished: None,
            tick: 0,
        }
    }

    fn log(&mut self, err: bool, msg: String) {
        self.logs.push_back((err, msg));
        while self.logs.len() > 200 {
            self.logs.pop_front();
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Plan { course, chapters } => {
                self.course = course;
                self.total_audio = chapters.iter().map(|c| c.audio_secs).sum();
                self.states = vec![ChState::Pending; chapters.len()];
                self.chapters = chapters;
            }
            Event::Chapter { idx, state } => {
                if let Some(s) = self.states.get_mut(idx) {
                    *s = state;
                }
                if state == ChState::Transcribing {
                    self.current_chapter = Some(idx);
                }
            }
            Event::VideoStart {
                idx,
                name,
                chunks,
                duration,
            } => {
                self.current_chapter = Some(idx);
                self.video = name;
                self.video_duration = duration;
                self.chunk = (0, chunks);
                self.transcript.clear();
                self.asr_started.get_or_insert_with(Instant::now);
            }
            Event::Chunk {
                done,
                total,
                audio_secs,
                start,
                text,
                cached,
            } => {
                self.chunk = (done, total);
                self.done_audio += audio_secs;
                if !cached {
                    self.fresh_audio += audio_secs;
                }
                if !text.is_empty() {
                    self.transcript.push_back((start, text));
                    while self.transcript.len() > 40 {
                        self.transcript.pop_front();
                    }
                }
            }
            Event::VideoDone => {}
            Event::Stage(s) => {
                self.log(false, format!("── {s} ──"));
                self.stage = s;
            }
            Event::SkippedAudio { secs } => self.total_audio = (self.total_audio - secs).max(0.0),
            Event::Log(m) => self.log(false, m),
            Event::Error(m) => self.log(true, m),
            Event::Finished { ok, out } => {
                let msg = if ok {
                    format!("All done. Results in {}", out.display())
                } else {
                    format!(
                        "Finished with problems. See the log. Results in {}",
                        out.display()
                    )
                };
                self.finished = Some((ok, msg));
            }
        }
    }

    fn eta(&self) -> Option<Duration> {
        let started = self.asr_started?;
        let elapsed = started.elapsed().as_secs_f64();
        if self.fresh_audio < 30.0 || elapsed < 5.0 {
            return None;
        }
        let speed = self.fresh_audio / elapsed;
        let left = (self.total_audio - self.done_audio).max(0.0);
        Some(Duration::from_secs_f64(left / speed))
    }
}

/// Runs the TUI until the pipeline finishes and the user quits.
pub fn run(rx: &Receiver<Event>, stop: &AtomicBool) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, rx, stop);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    rx: &Receiver<Event>,
    stop: &AtomicBool,
) -> Result<()> {
    let mut app = App::new();
    loop {
        while let Ok(ev) = rx.try_recv() {
            app.apply(ev);
        }
        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| draw(f, &app))?;
        if event::poll(Duration::from_millis(100))?
            && let TermEvent::Key(k) = event::read()?
        {
            let ctrl_c =
                k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
            if k.kind == KeyEventKind::Press
                && (k.code == KeyCode::Char('q') || k.code == KeyCode::Esc || ctrl_c)
            {
                stop.store(true, Ordering::Relaxed);
                return Ok(());
            }
        }
    }
}

fn panel(title: &str, color: Color) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(title, Style::default().fg(color).bold()),
            Span::raw(" "),
        ]))
        .padding(Padding::horizontal(1))
}

fn draw(f: &mut Frame, app: &App) {
    let [header, body, logs, footer] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(10),
        Constraint::Length(8),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_header(f, header, app);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).areas(body);
    draw_chapters(f, left, app);
    draw_now(f, right, app);
    draw_logs(f, logs, app);
    draw_footer(f, footer, app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [title_row, gauge_row, stats_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let title = Line::from(vec![
        Span::styled(" course", Style::default().fg(ACCENT).bold()),
        Span::styled("2", Style::default().fg(WARM).bold()),
        Span::styled("skill ", Style::default().fg(ACCENT2).bold()),
        Span::styled("│ ", Style::default().fg(BORDER)),
        Span::styled(
            app.course.as_str(),
            Style::default().fg(FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  │ ", Style::default().fg(BORDER)),
        Span::styled(app.stage.as_str(), Style::default().fg(WARM).bold()),
    ]);
    f.render_widget(Paragraph::new(title), title_row);

    let ratio = if app.total_audio > 0.0 {
        (app.done_audio / app.total_audio).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gauge = LineGauge::default()
        .filled_style(Style::default().fg(ACCENT))
        .unfilled_style(Style::default().fg(BORDER))
        .label(Span::styled(
            format!(" {:>5.1}% ", ratio * 100.0),
            Style::default().fg(FG).bold(),
        ))
        .ratio(ratio);
    f.render_widget(gauge, gauge_row);

    let skills_done = app.states.iter().filter(|s| **s == ChState::Done).count();
    let eta = app
        .eta()
        .map_or_else(|| "estimating…".to_string(), |d| fmt_ts(d.as_secs_f64()));
    let speed = app
        .asr_started
        .map(|s| s.elapsed().as_secs_f64())
        .filter(|e| *e > 1.0)
        .map_or(0.0, |e| app.fresh_audio / e);
    let stats = Line::from(vec![
        stat(
            "audio",
            format!("{} / {}", fmt_ts(app.done_audio), fmt_ts(app.total_audio)),
        ),
        stat("speed", format!("{speed:.1}× realtime")),
        stat("eta", eta),
        stat("elapsed", fmt_ts(app.started.elapsed().as_secs_f64())),
        stat(
            "units done",
            format!("{skills_done}/{}", app.chapters.len()),
        ),
    ]);
    f.render_widget(Paragraph::new(stats), stats_row);
}

fn stat(label: &str, value: String) -> Span<'static> {
    Span::styled(format!(" {label} {value}  "), Style::default().fg(DIM))
}

fn state_badge(state: ChState, tick: usize) -> (String, Color, &'static str) {
    let spin = SPINNER[tick % SPINNER.len()].to_string();
    match state {
        ChState::Pending => ("○".into(), DIM, "waiting"),
        ChState::Docs => (spin, WARM, "handouts"),
        ChState::Transcribing => (spin, ACCENT, "transcribing"),
        ChState::Transcribed => ("◆".into(), ACCENT, "transcribed"),
        ChState::SkillQueued => ("◇".into(), ACCENT2, "skill queued"),
        ChState::Skill => (spin, ACCENT2, "writing skill"),
        ChState::Done => ("✔".into(), OK, "done"),
        ChState::Failed => ("✘".into(), BAD, "failed"),
    }
}

fn draw_chapters(f: &mut Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .chapters
        .iter()
        .zip(&app.states)
        .enumerate()
        .map(|(i, (c, s))| {
            let (glyph, color, label) = state_badge(*s, app.tick);
            let active = app.current_chapter == Some(i) && *s == ChState::Transcribing;
            let name_style = if active {
                Style::default().fg(FG).bold()
            } else {
                Style::default().fg(FG)
            };
            let line1 = Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(color).bold()),
                Span::styled(c.name.clone(), name_style),
            ]);
            let line2 = Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{label} "), Style::default().fg(color)),
                Span::styled(
                    format!(
                        "· {} video{} · {} docs · {}",
                        c.videos,
                        if c.videos == 1 { "" } else { "s" },
                        c.docs,
                        fmt_ts(c.audio_secs)
                    ),
                    Style::default().fg(DIM),
                ),
            ]);
            ListItem::new(vec![line1, line2])
        })
        .collect();
    f.render_widget(List::new(items).block(panel("Units", ACCENT)), area);
}

fn draw_now(f: &mut Frame, area: Rect, app: &App) {
    let [top, bottom] = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(area);
    let block = panel("Now", WARM);
    let inner = block.inner(top);
    f.render_widget(block, top);
    let [name_row, gauge_row, _] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let name = if let Some((ok, msg)) = &app.finished {
        Line::from(Span::styled(
            msg.clone(),
            Style::default().fg(if *ok { OK } else { BAD }).bold(),
        ))
    } else if app.video.is_empty() {
        Line::from(Span::styled("Preparing…", Style::default().fg(DIM)))
    } else {
        Line::from(vec![
            Span::styled("▶ ", Style::default().fg(WARM)),
            Span::styled(app.video.clone(), Style::default().fg(FG).bold()),
            Span::styled(
                format!("  {}", fmt_ts(app.video_duration)),
                Style::default().fg(DIM),
            ),
        ])
    };
    f.render_widget(Paragraph::new(name), name_row);
    let (d, t) = app.chunk;
    let ratio = if t > 0 { d as f64 / t as f64 } else { 0.0 };
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(WARM).bg(Color::Rgb(40, 44, 56)))
            .label(Span::styled(
                format!("chunk {d}/{t}"),
                Style::default().fg(Color::Black).bold(),
            ))
            .ratio(ratio.clamp(0.0, 1.0)),
        gauge_row,
    );

    let height = bottom.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (start, text) in &app.transcript {
        lines.push(Line::from(vec![
            Span::styled(
                format!("[{}] ", fmt_ts(*start)),
                Style::default().fg(ACCENT2).bold(),
            ),
            Span::styled(text.clone(), Style::default().fg(FG)),
        ]));
    }
    // Rough scroll so the newest text stays in view after wrapping.
    let width = bottom.width.saturating_sub(4).max(1) as usize;
    let rows: usize = lines.iter().map(|l| l.width().div_ceil(width).max(1)).sum();
    let scroll = rows.saturating_sub(height) as u16;
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0))
        .block(panel("Live transcript", ACCENT2));
    f.render_widget(p, bottom);
}

fn draw_logs(f: &mut Frame, area: Rect, app: &App) {
    let height = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(height)
        .rev()
        .map(|(err, m)| {
            let (mark, color) = if *err { ("✘ ", BAD) } else { ("· ", DIM) };
            Line::from(vec![
                Span::styled(mark, Style::default().fg(color)),
                Span::styled(m.clone(), Style::default().fg(if *err { BAD } else { FG })),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(panel("Log", DIM)), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let hint = if app.finished.is_some() {
        "press q to exit"
    } else {
        "q quit (progress is saved, rerun to resume)"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" q ", Style::default().fg(Color::Black).bg(ACCENT).bold()),
            Span::styled(format!(" {hint}"), Style::default().fg(DIM)),
        ]))
        .alignment(Alignment::Left),
        area,
    );
}
