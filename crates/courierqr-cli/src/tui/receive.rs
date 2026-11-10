//! Receive tab: live camera preview, progress, pickup rate, rejection
//! breakdown, then auto-unlock with this computer's key (passphrase as the
//! fallback) and save to the downloads folder.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use courierqr_core::{Accepted, Error as CoreError, OpenKey, Receiver};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph, Sparkline, Wrap};

use super::theme::*;
use super::{App, SourceSpec, panel};
use crate::camera::{CameraInfo, Frame as CamFrame, Rejections, Source, Y4mSource};
use crate::decode;

// ---------------------------------------------------------------- worker

/// Downscaled grayscale snapshot for the in-terminal preview.
#[derive(Clone)]
pub(crate) struct Preview {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

pub(crate) struct FrameInfo {
    pub preview: Option<Preview>,
    pub frames: u64,
    pub codes: u64,
    pub new: u32,
    pub received: usize,
    pub needed: usize,
    pub rej: Rejections,
    /// Frames actually delivered per second (last ~2 s).
    pub capture_fps: f64,
    /// Average time to find and decode QR codes in one frame.
    pub decode_ms: f64,
}

pub(crate) enum Msg {
    Opened(Box<CameraInfo>),
    /// The full-resolution preview window opened (true) or closed (false),
    /// or couldn't open (Err).
    Window(std::result::Result<bool, String>),
    Frame(Box<FrameInfo>),
    Complete(Box<Receiver>),
    Error(String),
}

struct Worker {
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<Msg>,
}

fn spawn(spec: SourceSpec, window: Arc<AtomicBool>) -> Worker {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        if let Err(e) = work_with(spec, &tx, &flag, Some(&window)) {
            let _ = tx.send(Msg::Error(format!("{e:#}")));
        }
    });
    Worker { stop, rx }
}

#[cfg(feature = "camera")]
fn open_camera(dev: &str) -> Result<Box<dyn Source>> {
    Ok(Box::new(crate::camera::V4lSource::open(dev, None)?))
}

#[cfg(not(feature = "camera"))]
fn open_camera(_: &str) -> Result<Box<dyn Source>> {
    anyhow::bail!("this build has no webcam support")
}

/// Runs on its own thread: capture → decode → feed the receiver.
#[cfg(test)] // the app uses `work_with`; tests drive this directly
pub(crate) fn work(spec: SourceSpec, tx: &mpsc::Sender<Msg>, stop: &AtomicBool) -> Result<()> {
    work_with(spec, tx, stop, None)
}

/// [`work`], plus an optional full-resolution preview window that follows
/// `window` (the `v` key): it opens and closes on this thread, which owns
/// the frames, so the terminal UI never blocks on it.
fn work_with(
    spec: SourceSpec,
    tx: &mpsc::Sender<Msg>,
    stop: &AtomicBool,
    window: Option<&AtomicBool>,
) -> Result<()> {
    let mut view: Option<crate::camera::Preview> = None;
    let paced = matches!(spec, SourceSpec::File(_));
    let mut src: Box<dyn Source> = match &spec {
        SourceSpec::File(p) => Box::new(Y4mSource::open(p)?),
        SourceSpec::Camera(dev) => open_camera(dev)?,
    };
    let _ = tx.send(Msg::Opened(Box::new(src.info())));

    let mut rx = Receiver::new();
    let mut rej = Rejections::default();
    let (mut frames, mut codes) = (0u64, 0u64);
    let mut last_preview = Instant::now() - Duration::from_secs(1);
    let mut stamps: VecDeque<Instant> = VecDeque::new();
    let mut decode_ms = 0.0f64;

    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        let Some(f) = src.next_frame()? else {
            let _ = tx.send(Msg::Error(
                "the recording ended before the transfer completed".into(),
            ));
            return Ok(());
        };
        frames += 1;
        let now = Instant::now();
        stamps.push_back(now);
        while stamps
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(2))
        {
            stamps.pop_front();
        }
        let capture_fps = match (stamps.front(), stamps.len()) {
            (Some(first), n) if n > 1 => {
                (n - 1) as f64 / now.duration_since(*first).as_secs_f64().max(1e-3)
            }
            _ => 0.0,
        };
        let t_dec = Instant::now();
        let payloads = decode::decode_all(&f.pixels, f.width, f.height);
        let ms = t_dec.elapsed().as_secs_f64() * 1000.0;
        decode_ms = if frames == 1 {
            ms
        } else {
            decode_ms * 0.9 + ms * 0.1
        };
        let mut new = 0;
        for p in payloads {
            codes += 1;
            match rx.push(&p) {
                Ok(Accepted::Progress | Accepted::Complete) => new += 1,
                Ok(_) => {}
                Err(e) => rej.note(e, &p),
            }
        }
        let pr = rx.progress();
        if let Some(flag) = window {
            if flag.load(Ordering::Relaxed) {
                if view.is_none() {
                    match crate::camera::Preview::open_quiet() {
                        Ok(mut v) => {
                            v.title("camera (v in the terminal to close)");
                            view = Some(v);
                            let _ = tx.send(Msg::Window(Ok(true)));
                        }
                        Err(e) => {
                            flag.store(false, Ordering::Relaxed);
                            let _ = tx.send(Msg::Window(Err(e)));
                        }
                    }
                }
                let frac = pr.received as f32 / pr.needed.max(1) as f32;
                let closed = view.as_mut().is_some_and(|v| !v.show(&f, new > 0, frac));
                if closed {
                    // Closed with the window's own button or Esc.
                    view = None;
                    flag.store(false, Ordering::Relaxed);
                    let _ = tx.send(Msg::Window(Ok(false)));
                }
            } else if view.take().is_some() {
                let _ = tx.send(Msg::Window(Ok(false)));
            }
        }
        let preview = (last_preview.elapsed() >= Duration::from_millis(50)).then(|| {
            last_preview = Instant::now();
            downscale(&f, 192)
        });
        let info = FrameInfo {
            preview,
            frames,
            codes,
            new,
            received: pr.received,
            needed: pr.needed,
            rej: rej.clone(),
            capture_fps,
            decode_ms,
        };
        if tx.send(Msg::Frame(Box::new(info))).is_err() {
            return Ok(()); // UI went away
        }
        if pr.complete {
            let _ = tx.send(Msg::Complete(Box::new(rx)));
            return Ok(());
        }
        if paced {
            // Recordings play back at camera speed so the UI looks real.
            thread::sleep(Duration::from_millis(33).saturating_sub(t0.elapsed()));
        }
    }
    Ok(())
}

/// Mirrored (selfie-style), box-averaged thumbnail at most `max_w` wide.
fn downscale(f: &CamFrame, max_w: usize) -> Preview {
    let step = f.width.div_ceil(max_w).max(1);
    let (w, h) = (f.width / step, f.height / step);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let sx = (w - 1 - x) * step; // mirrored block start
            let mut sum = 0u32;
            for dy in 0..step {
                let row = (y * step + dy) * f.width;
                for dx in 0..step {
                    sum += f.pixels[row + sx + dx] as u32;
                }
            }
            px.push((sum / (step * step) as u32) as u8);
        }
    }
    Preview { w, h, px }
}

// ---------------------------------------------------------------- state

pub(crate) enum Event {
    Message(String, bool),
}

pub(crate) enum Phase {
    Idle,
    Scanning,
    Unlock {
        rx: Box<Receiver>,
        input: String,
        error: Option<String>,
        note: String,
    },
    Done {
        name: String,
        path: PathBuf,
        size: usize,
        text: Option<String>,
    },
    Failed(String),
}

pub(crate) struct ReceiveTab {
    sources: Vec<(SourceSpec, String)>,
    current: usize,
    pub(crate) phase: Phase,
    worker: Option<Worker>,
    pub(crate) preview: Option<Preview>,
    pub(crate) frames: u64,
    pub(crate) codes: u64,
    pub(crate) received: usize,
    pub(crate) needed: usize,
    pub(crate) rej: Rejections,
    /// New pieces per second, last 60 s (newest last).
    pub(crate) rate: VecDeque<u64>,
    bucket_start: Instant,
    pub(crate) started: Option<Instant>,
    elapsed: Duration,
    flash_until: Instant,
    completed: Option<Box<Receiver>>,
    pub(crate) info: Option<CameraInfo>,
    pub(crate) capture_fps: f64,
    pub(crate) decode_ms: f64,
    /// Public feed slideshow, shown whenever the camera is idle.
    pub(crate) feed: super::feed::FeedView,
    /// Full-resolution camera window wanted (`v`); the worker follows it.
    pub(crate) window: Arc<AtomicBool>,
}

impl ReceiveTab {
    pub(crate) fn new(sources: Vec<(SourceSpec, String)>) -> Self {
        ReceiveTab {
            sources,
            current: 0,
            phase: Phase::Idle,
            worker: None,
            preview: None,
            frames: 0,
            codes: 0,
            received: 0,
            needed: 0,
            rej: Rejections::default(),
            rate: VecDeque::from(vec![0; 60]),
            bucket_start: Instant::now(),
            started: None,
            elapsed: Duration::ZERO,
            flash_until: Instant::now(),
            completed: None,
            info: None,
            capture_fps: 0.0,
            decode_ms: 0.0,
            feed: super::feed::FeedView::new(),
            window: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Camera idle and messages on the feed: the panel plays the feed.
    pub(crate) fn showing_feed(&self) -> bool {
        matches!(self.phase, Phase::Idle) && self.feed.len() > 0
    }

    pub(crate) fn wants_text(&self) -> bool {
        matches!(self.phase, Phase::Unlock { .. })
    }

    pub(crate) fn source_label(&self) -> (&'static str, String) {
        let dot = if matches!(self.phase, Phase::Scanning) {
            "●"
        } else {
            "○"
        };
        let name = match &self.info {
            Some(i) => format!(
                "{} · {}×{} {}{}",
                i.card,
                i.width,
                i.height,
                i.format,
                i.fps.map(|f| format!(" · {f:.0} fps")).unwrap_or_default()
            ),
            None => self
                .sources
                .get(self.current)
                .map(|s| s.1.clone())
                .unwrap_or_else(|| "none found".into()),
        };
        (dot, name)
    }

    pub(crate) fn next_camera(&mut self) -> String {
        if matches!(self.phase, Phase::Scanning) {
            return "stop scanning before switching cameras".into();
        }
        if self.sources.len() < 2 {
            return "only one camera available".into();
        }
        self.current = (self.current + 1) % self.sources.len();
        self.info = None;
        format!("camera: {}", self.sources[self.current].1)
    }

    fn start(&mut self) -> Result<(), String> {
        let Some((spec, _)) = self.sources.get(self.current).cloned() else {
            return Err("no camera found. Plug one in, or check `courierqr cameras`".into());
        };
        let info = self.info.take();
        // Fresh counters, but keep the feed's place and the window toggle.
        let feed = std::mem::replace(&mut self.feed, super::feed::FeedView::new());
        let window = self.window.clone();
        *self = ReceiveTab {
            sources: std::mem::take(&mut self.sources),
            current: self.current,
            info,
            feed,
            window,
            ..ReceiveTab::new(vec![])
        };
        self.worker = Some(spawn(spec, self.window.clone()));
        self.phase = Phase::Scanning;
        self.started = Some(Instant::now());
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        if let Some(w) = self.worker.take() {
            w.stop.store(true, Ordering::Relaxed);
        }
        if matches!(self.phase, Phase::Scanning) {
            self.phase = Phase::Idle;
        }
    }

    pub(crate) fn take_completed(&mut self) -> Option<Box<Receiver>> {
        self.completed.take()
    }

    /// Drain the worker; roll the per-second pickup buckets.
    pub(crate) fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while self.bucket_start.elapsed() >= Duration::from_secs(1) {
            self.bucket_start += Duration::from_secs(1);
            self.rate.pop_front();
            self.rate.push_back(0);
        }
        if let (Phase::Scanning, Some(t)) = (&self.phase, self.started) {
            self.elapsed = t.elapsed();
        }
        let Some(w) = &self.worker else { return events };
        let msgs: Vec<Msg> = w.rx.try_iter().collect();
        let mut finished = false;
        for m in msgs {
            match m {
                Msg::Opened(info) => self.info = Some(*info),
                Msg::Frame(i) => self.apply(*i),
                Msg::Complete(rx) => {
                    self.completed = Some(rx);
                    finished = true;
                }
                Msg::Error(e) => {
                    self.phase = Phase::Failed(e.clone());
                    events.push(Event::Message(e, true));
                    finished = true;
                }
                Msg::Window(Ok(true)) => {
                    events.push(Event::Message("camera window open".into(), false))
                }
                Msg::Window(Ok(false)) => {
                    events.push(Event::Message("camera window closed".into(), false))
                }
                Msg::Window(Err(e)) => events.push(Event::Message(e, true)),
            }
        }
        if finished {
            self.worker = None;
        }
        events
    }

    pub(crate) fn apply(&mut self, i: FrameInfo) {
        if let Some(p) = i.preview {
            self.preview = Some(p);
        }
        self.frames = i.frames;
        self.codes = i.codes;
        self.received = i.received;
        self.needed = i.needed;
        self.rej = i.rej;
        self.capture_fps = i.capture_fps;
        self.decode_ms = i.decode_ms;
        if i.new > 0 {
            self.flash_until = Instant::now() + Duration::from_millis(150);
            if let Some(last) = self.rate.back_mut() {
                *last += i.new as u64;
            }
        }
    }
}

// ---------------------------------------------------------------- actions

pub(crate) fn start_stop(app: &mut App) {
    match app.receive.phase {
        Phase::Scanning => {
            app.receive.stop();
            app.toast("camera stopped", false);
        }
        Phase::Unlock { .. } => {}
        _ => {
            if let Err(e) = app.receive.start() {
                app.toast(e, true);
            }
        }
    }
}

/// Transfer complete: try this computer's own key first, silently.
pub(crate) fn unlock_with_own_key(app: &mut App, rx: Box<Receiver>) {
    app.receive.elapsed = app.receive.started.map(|t| t.elapsed()).unwrap_or_default();
    let note;
    match crate::pair::own_identity() {
        Ok(Some(secret)) => match rx.open(&OpenKey::identity(secret)).expect("complete") {
            Ok(file) => return save(app, file.name, file.data),
            Err(CoreError::Decrypt(_)) => {
                note = "Not sent to this computer's key: enter the passphrase.".to_string()
            }
            Err(e) => {
                app.receive.phase = Phase::Failed(e.to_string());
                return;
            }
        },
        _ => note = "Enter the passphrase it was sent with.".to_string(),
    }
    app.receive.phase = Phase::Unlock {
        rx,
        input: String::new(),
        error: None,
        note,
    };
}

fn save(app: &mut App, name: String, data: Vec<u8>) {
    match crate::save_unique(&app.out, &name, &data) {
        Ok(path) => {
            let text = std::str::from_utf8(&data)
                .ok()
                .filter(|t| {
                    !t.chars()
                        .take(4096)
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                })
                .map(|t| t.lines().take(12).collect::<Vec<_>>().join("\n"));
            app.toast(
                format!(
                    "saved {} ({} bytes)",
                    super::shorten_home(&path),
                    data.len()
                ),
                false,
            );
            app.receive.phase = Phase::Done {
                name,
                path,
                size: data.len(),
                text,
            };
        }
        Err(e) => app.receive.phase = Phase::Failed(format!("couldn't save: {e:#}")),
    }
}

pub(crate) fn on_key(app: &mut App, k: KeyEvent) {
    if let Phase::Unlock { input, error, .. } = &mut app.receive.phase {
        match k.code {
            KeyCode::Esc => {
                app.receive.phase = Phase::Idle;
                app.toast("discarded", false);
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => {
                input.push(c);
                *error = None;
            }
            KeyCode::Enter => {
                let Phase::Unlock { rx, input, .. } =
                    std::mem::replace(&mut app.receive.phase, Phase::Idle)
                else {
                    unreachable!()
                };
                match rx
                    .open(&OpenKey::passphrase(input.clone()))
                    .expect("complete")
                {
                    Ok(file) => save(app, file.name, file.data),
                    Err(e) => {
                        let msg = match e {
                            CoreError::Decrypt(_) => {
                                "That didn't unlock it. Wrong passphrase.".to_string()
                            }
                            other => other.to_string(),
                        };
                        let note = "Enter the passphrase it was sent with.".to_string();
                        app.receive.phase = Phase::Unlock {
                            rx,
                            input: String::new(),
                            error: Some(msg),
                            note,
                        };
                    }
                }
            }
            _ => {}
        }
        return;
    }
    match k.code {
        KeyCode::Enter | KeyCode::Char(' ') => start_stop(app),
        KeyCode::Char('c') => {
            let m = app.receive.next_camera();
            app.toast(m, false);
        }
        KeyCode::Char('v') => {
            let on = !app.receive.window.load(Ordering::Relaxed);
            app.receive.window.store(on, Ordering::Relaxed);
            let msg = match (on, matches!(app.receive.phase, Phase::Scanning)) {
                (true, true) => "opening the camera window…",
                (true, false) => "camera window will open when the camera starts",
                (false, _) => "camera window off",
            };
            app.toast(msg, false);
        }
        _ => {}
    }
}

pub(crate) fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.receive.phase {
        Phase::Idle | Phase::Failed(_) => vec![
            ("enter", "start camera"),
            ("c", "switch camera"),
            ("v", "camera window"),
        ],
        Phase::Scanning => vec![("enter", "stop"), ("v", "camera window")],
        Phase::Unlock { .. } => vec![
            ("type", "passphrase"),
            ("enter", "unlock"),
            ("esc", "discard"),
        ],
        Phase::Done { .. } => vec![("enter", "receive another")],
    }
}

// ---------------------------------------------------------------- drawing

pub(crate) fn draw(app: &App, f: &mut Frame, area: Rect) {
    let r = &app.receive;
    let [left, right] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(48)]).areas(area);
    // Idle camera + messages on the feed: the panel plays the public feed,
    // and gets the whole column (camera details hide until you scan).
    let show_feed = r.showing_feed();
    let [left, details] = Layout::vertical([
        Constraint::Min(6),
        Constraint::Length(if show_feed { 0 } else { 6 }),
    ])
    .areas(left);
    if !show_feed {
        draw_camera_details(app, f, details);
    }

    // Camera view
    let flash = Instant::now() < r.flash_until;
    let title = match r.phase {
        Phase::Scanning => "camera · live",
        _ if show_feed => "public feed",
        _ => "camera",
    };
    let block = panel(title).border_style(Style::new().fg(if flash { ACCENT } else { LINE }));
    let inner = block.inner(left);
    f.render_widget(block, left);
    if show_feed {
        super::feed::draw(&r.feed, f, inner);
    } else {
        match (&r.preview, &r.phase) {
            (Some(p), Phase::Scanning) => draw_preview(f, inner, p),
            _ => {
                let msg: Vec<Line> = match &r.phase {
                    Phase::Idle => vec![
                        Line::from(Span::styled(
                            "Press Enter to start the camera",
                            Style::new().fg(TEXT),
                        )),
                        Line::raw(""),
                        Line::from(Span::styled(
                            "On the phone: Send → choose a file or type text → Start sending,",
                            Style::new().fg(MUTED),
                        )),
                        Line::from(Span::styled(
                            "then hold the screen 20–30 cm from the webcam.",
                            Style::new().fg(MUTED),
                        )),
                        Line::raw(""),
                        Line::from(Span::styled(
                            "Public feed: no messages yet. It plays here once there are some.",
                            Style::new().fg(MUTED),
                        )),
                    ],
                    Phase::Scanning => vec![Line::from(Span::styled(
                        "opening camera…",
                        Style::new().fg(MUTED),
                    ))],
                    Phase::Failed(e) => vec![
                        Line::from(Span::styled(e.clone(), Style::new().fg(ERROR))),
                        Line::raw(""),
                        Line::from(Span::styled("Enter to try again", Style::new().fg(MUTED))),
                    ],
                    Phase::Unlock { .. } => vec![Line::from(Span::styled(
                        "received everything ✓",
                        Style::new().fg(ACCENT),
                    ))],
                    Phase::Done { .. } => {
                        vec![Line::from(Span::styled("done ✓", Style::new().fg(ACCENT)))]
                    }
                };
                let pad = (inner.height as usize).saturating_sub(msg.len()) / 2;
                let mut lines = vec![Line::raw(""); pad];
                lines.extend(msg);
                f.render_widget(Paragraph::new(lines).centered(), inner);
            }
        }
    }

    // Right column
    let bottom_h = match r.phase {
        Phase::Unlock { .. } => 9,
        Phase::Done { .. } => 14,
        _ => 0,
    };
    let [transfer, pickup, reads, bottom] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Min(5),
        Constraint::Length(bottom_h),
    ])
    .areas(right);

    // Transfer
    let block = panel("transfer");
    let tin = block.inner(transfer);
    f.render_widget(block, transfer);
    let state = match (&r.phase, r.needed) {
        (Phase::Idle, _) => ("ready", MUTED),
        (Phase::Scanning, 0) => ("looking for codes…", WARN),
        (Phase::Scanning, _) => ("receiving", ACCENT),
        (Phase::Unlock { .. }, _) => ("needs a passphrase", WARN),
        (Phase::Done { .. }, _) => ("complete", ACCENT),
        (Phase::Failed(_), _) => ("stopped", ERROR),
    };
    let ratio = if matches!(r.phase, Phase::Done { .. } | Phase::Unlock { .. }) {
        1.0
    } else if r.needed > 0 {
        (r.received as f64 / r.needed as f64).min(0.99)
    } else {
        0.0
    };
    let [s1, g, s2] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .areas(tin);
    f.render_widget(
        Paragraph::new(Span::styled(
            state.0,
            Style::new().fg(state.1).add_modifier(Modifier::BOLD),
        )),
        s1,
    );
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(Span::styled(
                format!("{:.0}%", ratio * 100.0),
                Style::new().fg(TEXT),
            ))
            .gauge_style(Style::new().fg(ACCENT).bg(LINE)),
        g,
    );
    let pieces = if r.needed > 0 {
        format!("{} / ~{} pieces", r.received, r.needed)
    } else {
        "– pieces".into()
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled(pieces, Style::new().fg(TEXT)),
                Span::styled(
                    format!("   {:.0} s", r.elapsed.as_secs_f64()),
                    Style::new().fg(MUTED),
                ),
            ]),
        ]),
        s2,
    );

    // Pickup rate
    let block = panel("pickup · pieces per second, last 60 s");
    let pin = block.inner(pickup);
    f.render_widget(block, pickup);
    let [spark, txt] = Layout::vertical([Constraint::Min(2), Constraint::Length(1)]).areas(pin);
    // Newest samples on the right: keep only as many as fit.
    let fit = spark.width as usize;
    let data: Vec<u64> = r
        .rate
        .iter()
        .skip(r.rate.len().saturating_sub(fit))
        .copied()
        .collect();
    f.render_widget(
        Sparkline::default()
            .data(&data)
            .style(Style::new().fg(ACCENT)),
        spark,
    );
    let recent: u64 = r.rate.iter().rev().skip(1).take(5).sum();
    let peak = r.rate.iter().max().copied().unwrap_or(0);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(
                "{:.1}/s over the last 5 s · peak {peak}/s",
                recent as f64 / 5.0
            ),
            Style::new().fg(MUTED),
        )),
        txt,
    );

    // Reads + rejection breakdown
    let block = panel("reads");
    let rin = block.inner(reads);
    f.render_widget(block, reads);
    let row = |k: &str, v: String, c: Color| {
        Line::from(vec![
            Span::styled(format!("{k:<18}"), Style::new().fg(MUTED)),
            Span::styled(v, Style::new().fg(c)),
        ])
    };
    let mut lines = vec![
        row("camera frames", r.frames.to_string(), TEXT),
        row("QR codes read", r.codes.to_string(), TEXT),
        row("transfer pieces", r.received.to_string(), ACCENT),
    ];
    if r.rej.not_courierqr > 0 {
        let what = match r.rej.example.as_deref() {
            Some(t) if t.to_ascii_lowercase().starts_with("age1") => {
                "pairing key: phone is on Keys, use Send".to_string()
            }
            Some(t) => format!("other QR (\"{t}…\")"),
            None => "other QR codes".into(),
        };
        lines.push(row(
            "not a transfer",
            format!("{} · {what}", r.rej.not_courierqr),
            WARN,
        ));
    }
    if r.rej.damaged > 0 {
        lines.push(row(
            "damaged",
            format!("{} (checksum failed)", r.rej.damaged),
            ERROR,
        ));
    }
    if r.rej.other > 0 {
        lines.push(row("unsupported", r.rej.other.to_string(), ERROR));
    }
    f.render_widget(Paragraph::new(lines), rin);

    // Unlock / result
    match &r.phase {
        Phase::Unlock {
            input, error, note, ..
        } => {
            let block = panel("unlock").border_style(Style::new().fg(WARN));
            let uin = block.inner(bottom);
            f.render_widget(block, bottom);
            let mut lines = vec![
                Line::from(Span::styled(note.clone(), Style::new().fg(TEXT))),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("passphrase  ", Style::new().fg(MUTED)),
                    Span::styled("•".repeat(input.chars().count()), Style::new().fg(TEXT)),
                    Span::styled("▏", Style::new().fg(ACCENT)),
                ]),
            ];
            if let Some(e) = error {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(ERROR))));
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), uin);
        }
        Phase::Done {
            name,
            path,
            size,
            text,
        } => {
            let block = panel("received").border_style(Style::new().fg(ACCENT));
            let din = block.inner(bottom);
            f.render_widget(block, bottom);
            let mut lines = vec![
                Line::from(Span::styled(
                    name.clone(),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("{size} bytes → {}", super::shorten_home(path)),
                    Style::new().fg(MUTED),
                )),
                Line::raw(""),
            ];
            match text {
                Some(t) => lines.extend(
                    t.lines()
                        .map(|l| Line::from(Span::styled(l.to_owned(), Style::new().fg(TEXT)))),
                ),
                None => lines.push(Line::from(Span::styled(
                    "(binary file)",
                    Style::new().fg(MUTED),
                ))),
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), din);
        }
        _ => {}
    }
}

/// Device, stream and sensor details: the numbers that decide whether a
/// camera can read the codes, plus a one-line read on the bottleneck.
fn draw_camera_details(app: &App, f: &mut Frame, area: Rect) {
    let r = &app.receive;
    let block = panel("camera details");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let k = |s: &str| Span::styled(format!("{s:<8}"), Style::new().fg(MUTED));
    let v = |s: String| Span::styled(s, Style::new().fg(TEXT));
    let a = |s: String| Span::styled(s, Style::new().fg(ACCENT));
    let Some(i) = &r.info else {
        let names: Vec<String> = r.sources.iter().map(|s| s.1.clone()).collect();
        let lines = vec![
            Line::from(vec![
                k("found"),
                v(if names.is_empty() {
                    "no cameras".into()
                } else {
                    names.join("  ·  ")
                }),
            ]),
            Line::from(Span::styled(
                "Start the camera to see its resolution, frame rate and focus.",
                Style::new().fg(MUTED),
            )),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    };
    let mp = CameraInfo::megapixels(i.width, i.height);
    let mut stream = vec![
        k("stream"),
        a(format!("{}×{}", i.width, i.height)),
        v(format!(" ({mp:.1} MP) {} · ", i.format)),
        v(match i.fps {
            Some(f) => format!("{f:.0} fps set · "),
            None => "fps not reported · ".into(),
        }),
    ];
    let live = matches!(r.phase, Phase::Scanning) && r.frames > 1;
    if live {
        stream.push(a(format!("{:.1} fps measured", r.capture_fps)));
        stream.push(v(format!(" · decode {:.0} ms/frame", r.decode_ms)));
    }
    let max = match i.max {
        Some((w, h)) => format!("max {w}×{h} ({:.1} MP)", CameraInfo::megapixels(w, h)),
        None => "max unknown".into(),
    };
    let focus = match i.autofocus {
        Some(true) => "autofocus: yes",
        Some(false) => "focus: manual only",
        None => "focus: fixed",
    };
    // What's limiting the read rate right now?
    let verdict = if !live {
        None
    } else if let Some(set) = i.fps.filter(|s| r.capture_fps < s * 0.7) {
        if r.decode_ms > 1000.0 / set {
            Some((
                "decoding is the bottleneck: frames arrive faster than they can be scanned",
                WARN,
            ))
        } else {
            Some((
                "camera is delivering fewer frames than set (low light lowers webcam fps)",
                WARN,
            ))
        }
    } else if r.codes == 0 && r.frames > 60 {
        Some((
            "no QR codes seen yet: move the phone closer, brightness up, avoid glare",
            WARN,
        ))
    } else {
        Some(("camera and decoder keeping up", ACCENT))
    };
    let mut lines = vec![
        Line::from(vec![
            k("device"),
            v(format!(
                "{} · {} · {} · {}",
                i.card, i.device, i.driver, i.bus
            )),
        ]),
        Line::from(stream),
        Line::from(vec![
            k("sensor"),
            v(format!("{max} · formats {} · {focus}", i.formats.join(" "))),
        ]),
    ];
    if let Some((msg, color)) = verdict {
        lines.push(Line::from(vec![
            k("status"),
            Span::styled(msg, Style::new().fg(color)),
        ]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// Half-block rendering: each terminal cell shows two vertically stacked
/// pixels (▀ with fg = top, bg = bottom). Aspect-correct, letterboxed.
fn draw_preview(f: &mut Frame, area: Rect, p: &Preview) {
    if area.width == 0 || area.height == 0 || p.w == 0 || p.h == 0 {
        return;
    }
    let (cw, ch) = (area.width as f64, area.height as f64 * 2.0);
    let s = (cw / p.w as f64).min(ch / p.h as f64);
    let (dw, dh) = ((p.w as f64 * s) as u16, (p.h as f64 * s) as u16);
    let ox = area.x + (area.width - dw) / 2;
    let oy = area.y + (area.height - dh.div_ceil(2)) / 2;
    let buf = f.buffer_mut();
    // Average every source pixel that falls under an output pixel (box
    // filter) instead of picking one: far less jagged and noisy when the
    // panel is smaller than the preview.
    let sample = |x: u16, y: u16| -> u8 {
        let x0 = ((x as f64 / s) as usize).min(p.w - 1);
        let y0 = ((y as f64 / s) as usize).min(p.h - 1);
        let x1 = (((x + 1) as f64 / s) as usize).clamp(x0 + 1, p.w);
        let y1 = (((y + 1) as f64 / s) as usize).clamp(y0 + 1, p.h);
        let mut sum = 0u32;
        for sy in y0..y1 {
            for sx in x0..x1 {
                sum += p.px[sy * p.w + sx] as u32;
            }
        }
        (sum / ((x1 - x0) * (y1 - y0)) as u32) as u8
    };
    for cy in 0..dh.div_ceil(2) {
        for cx in 0..dw {
            let top = sample(cx, cy * 2);
            let bot = if cy * 2 + 1 < dh {
                sample(cx, cy * 2 + 1)
            } else {
                0
            };
            if let Some(cell) = buf.cell_mut((ox + cx, oy + cy)) {
                cell.set_symbol("▀")
                    .set_fg(Color::Rgb(top, top, top))
                    .set_bg(Color::Rgb(bot, bot, bot));
            }
        }
    }
}
