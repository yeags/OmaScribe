use crate::{
    audio::{self, Effects, Engine},
    session::{Kind, Marker, Session},
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const TEAL: Color32 = Color32::from_rgb(77, 211, 181);
const GOLD: Color32 = Color32::from_rgb(245, 190, 91);
const MAX_ZOOM: f64 = 128.;

#[derive(Clone, Copy)]
enum WaveDrag {
    Marker { index: usize, grab_offset: f64 },
    Select { start: f64 },
    Start { grab_offset: f64 },
    End { grab_offset: f64 },
}

#[derive(Default)]
struct PlaybackBuffers {
    original: Arc<Vec<f32>>,
    modified: Arc<Vec<f32>>,
}

type Loaded = Result<(Session, PlaybackBuffers, Vec<f32>, f64), String>;

enum FileResult {
    Open(Option<PathBuf>),
    Save(Result<Option<(PathBuf, String)>, String>),
}

pub struct App {
    engine: Option<Engine>,
    errors: mpsc::Receiver<String>,
    result: Option<mpsc::Receiver<Loaded>>,
    file_result: Option<mpsc::Receiver<FileResult>>,
    continue_after_save: bool,
    session: Session,
    saved: String,
    session_path: Option<PathBuf>,
    wave: Vec<f32>,
    duration: f64,
    draft: Effects,
    status: String,
    zoom: f64,
    offset: f64,
    drag: Option<WaveDrag>,
    marker_anchor: Option<usize>,
    marker_end: Option<usize>,
    buffers: PlaybackBuffers,
    bypass: bool,
    help: bool,
    undo: Vec<Vec<Marker>>,
    redo: Vec<Vec<Marker>>,
    pending_open: Option<PathBuf>,
    close_dialog: bool,
    allow_close: bool,
    load_path: Option<PathBuf>,
    resume: bool,
    resume_position: f64,
    theme_check: Instant,
}
impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = Color32::from_rgb(20, 25, 32);
        style.visuals.window_fill = Color32::from_rgb(27, 33, 42);
        style.visuals.selection.bg_fill = TEAL.gamma_multiply(0.35);
        style.spacing.item_spacing = Vec2::new(10., 10.);
        cc.egui_ctx.set_style(style);
        let (tx, errors) = mpsc::channel();
        let (engine, status) = match Engine::new(tx) {
            Ok(e) => (Some(e), "Open an audio file to begin.".into()),
            Err(e) => (None, e),
        };
        let pending_open = std::env::args_os().nth(1).map(PathBuf::from);
        Self {
            engine,
            errors,
            result: None,
            file_result: None,
            continue_after_save: false,
            session: Session::default(),
            saved: String::new(),
            session_path: None,
            wave: vec![],
            duration: 0.,
            draft: Effects::default(),
            status,
            zoom: 1.,
            offset: 0.,
            drag: None,
            marker_anchor: None,
            marker_end: None,
            buffers: PlaybackBuffers::default(),
            bypass: false,
            help: false,
            undo: vec![],
            redo: vec![],
            pending_open,
            close_dialog: false,
            allow_close: false,
            load_path: None,
            resume: false,
            resume_position: 0.,
            theme_check: Instant::now() - Duration::from_secs(3),
        }
    }
    fn dirty(&self) -> bool {
        self.duration > 0. && serde_json::to_string(&self.session).unwrap_or_default() != self.saved
    }
    fn busy(&self) -> bool {
        self.result.is_some() || self.file_result.is_some()
    }
    fn ready(&self) -> bool {
        self.duration > 0. && !self.busy()
    }
    fn position(&self) -> f64 {
        self.engine
            .as_ref()
            .map(|e| e.state.lock().unwrap().position())
            .unwrap_or(0.)
    }
    fn seek(&self, t: f64) {
        if let Some(e) = &self.engine {
            e.state.lock().unwrap().seek(t);
        }
    }
    fn toggle(&self) {
        if !self.ready() {
            return;
        }
        if let Some(e) = &self.engine {
            let mut t = e.state.lock().unwrap();
            if !t.playing
                && (t.position() >= t.duration
                    || (t.looping
                        && t.selection
                            .is_some_and(|(a, b)| t.position() < a || t.position() >= b)))
            {
                let start = t.selection.filter(|_| t.looping).map(|s| s.0).unwrap_or(0.);
                t.seek(start);
            }
            t.playing = !t.playing;
        }
    }
    fn checkpoint(&mut self) {
        self.undo.push(self.session.markers.clone());
        self.redo.clear();
    }
    fn marker(&mut self, kind: Kind) {
        if !self.ready() {
            return;
        }
        let time = self.position().min(self.duration);
        self.forget_marker_selection();
        self.drag = None;
        self.checkpoint();
        self.session.markers.push(Marker { time, kind });
        self.session
            .markers
            .sort_by(|a, b| a.time.total_cmp(&b.time));
    }
    fn forget_marker_selection(&mut self) {
        self.marker_anchor = None;
        self.marker_end = None;
    }
    fn select_marker(&mut self, index: usize, extend: bool) {
        if extend
            && let Some(anchor) = self.marker_anchor
            && anchor != index
        {
            self.marker_end = Some(index);
            self.update_marker_range();
            if let Some((start, _)) = self.session.selection {
                self.seek(start);
            }
            self.sync_selection();
        } else {
            self.marker_anchor = Some(index);
            self.marker_end = None;
            self.seek(self.session.markers[index].time);
        }
    }
    fn update_marker_range(&mut self) {
        if let (Some(a), Some(b)) = (self.marker_anchor, self.marker_end) {
            let (a, b) = (self.session.markers[a].time, self.session.markers[b].time);
            self.session.selection = (a != b).then_some((a.min(b), a.max(b)));
        }
    }
    fn move_marker(&mut self, index: usize, time: f64) -> usize {
        let markers = &mut self.session.markers;
        markers[index].time = time;
        let mut dest = index;
        while dest > 0 && markers[dest - 1].time > time {
            dest -= 1;
        }
        while dest + 1 < markers.len() && markers[dest + 1].time < time {
            dest += 1;
        }
        if dest != index {
            let marker = markers.remove(index);
            markers.insert(dest, marker);
            for selected in [&mut self.marker_anchor, &mut self.marker_end]
                .into_iter()
                .flatten()
            {
                if *selected == index {
                    *selected = dest;
                } else if (dest..index).contains(selected) {
                    *selected += 1;
                } else if (index + 1..=dest).contains(selected) {
                    *selected -= 1;
                }
            }
        }
        self.update_marker_range();
        dest
    }
    fn toggle_bypass(&mut self) {
        self.bypass = !self.bypass;
        if let Some(e) = &self.engine {
            let mut t = e.state.lock().unwrap();
            let position = t.position();
            if self.bypass {
                t.set_audio(self.buffers.original.clone(), 1., position);
            } else {
                t.set_audio(
                    self.buffers.modified.clone(),
                    self.session.effects.speed,
                    position,
                );
            }
        }
    }
    fn sync_selection(&self) {
        if let Some(e) = &self.engine {
            let mut t = e.state.lock().unwrap();
            t.selection = self.session.selection;
            t.looping = self.session.looping;
            if t.playing
                && t.looping
                && let Some((a, b)) = t.selection
                && (t.position() < a || t.position() >= b)
            {
                t.seek(a);
            }
        }
    }
    fn load(&mut self, path: PathBuf) {
        let session = if path.extension().is_some_and(|s| s == "omascribe") {
            match Session::read(&path) {
                Ok(s) => {
                    self.load_path = Some(path);
                    s
                }
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        } else {
            self.load_path = None;
            Session {
                audio: path,
                ..Default::default()
            }
        };
        self.start_render(session, true);
    }
    fn start_render(&mut self, session: Session, fresh: bool) {
        if self.busy() {
            return;
        }
        let Some(engine) = &self.engine else {
            self.status =
                "No audio device. Connect an output device, then restart OmaScribe.".into();
            return;
        };
        {
            let mut t = engine.state.lock().unwrap();
            self.resume = !fresh && t.playing;
            self.resume_position = if fresh { 0. } else { t.position() };
            t.playing = false;
        }
        self.status = if fresh {
            "Loading audio and building waveform…"
        } else {
            "Rendering tempo and EQ…"
        }
        .into();
        let rate = engine.rate;
        let existing_wave = if fresh {
            None
        } else {
            Some((
                self.wave.clone(),
                self.duration,
                self.buffers.original.clone(),
            ))
        };
        let (tx, rx) = mpsc::channel();
        self.result = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let path = std::fs::canonicalize(&session.audio)
                    .map_err(|e| format!("Cannot open {}: {e}", session.audio.display()))?;
                let mut session = session;
                session.audio = path;
                let (wave, duration, original) = if let Some(existing) = existing_wave {
                    existing
                } else {
                    let original = audio::decode(&session.audio, rate, &Effects::default())?;
                    let duration = original.len() as f64 / (rate as f64 * 2.);
                    (audio::peaks(&original), duration, Arc::new(original))
                };
                session.markers.retain(|m| m.time <= duration);
                session.selection = session.selection.and_then(|(a, b)| {
                    let b = b.min(duration);
                    (a < b).then_some((a, b))
                });
                let modified = if session.effects == Effects::default() {
                    original.clone()
                } else {
                    Arc::new(audio::decode(&session.audio, rate, &session.effects)?)
                };
                Ok((
                    session,
                    PlaybackBuffers { original, modified },
                    wave,
                    duration,
                ))
            })();
            let _ = tx.send(result);
        });
        if fresh {
            self.duration = 0.;
            self.wave.clear();
            self.buffers = PlaybackBuffers::default();
            self.forget_marker_selection();
            self.drag = None;
            self.undo.clear();
            self.redo.clear();
            self.zoom = 1.;
            self.offset = 0.;
        }
    }
    // Native portal dialogs wait for user input. Keep that wait, and disk I/O,
    // off the event-loop thread so Wayland compositor pings still get answered.
    fn save(&mut self, choose: bool) -> bool {
        if !self.ready() {
            return false;
        }
        let session = self.session.clone();
        let path = self.session_path.clone().filter(|_| !choose);
        let (tx, rx) = mpsc::channel();
        self.file_result = Some(rx);
        self.status = "Saving session…".into();
        std::thread::spawn(move || {
            let result = (|| {
                let path = path.or_else(|| {
                    rfd::FileDialog::new()
                        .add_filter("OmaScribe session", &["omascribe"])
                        .set_file_name("transcription.omascribe")
                        .save_file()
                });
                let Some(path) = path else { return Ok(None) };
                let path = path.with_extension("omascribe");
                session.save(&path)?;
                Ok(Some((path, serde_json::to_string(&session).unwrap())))
            })();
            let _ = tx.send(FileResult::Save(result));
        });
        true
    }
    fn pick_open(&mut self) {
        if self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.file_result = Some(rx);
        self.status = "Choose an audio file or session…".into();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter(
                    "Audio or OmaScribe session",
                    &[
                        "wav",
                        "mp3",
                        "ogg",
                        "flac",
                        "m4a",
                        "aac",
                        "opus",
                        "aiff",
                        "omascribe",
                    ],
                )
                .add_filter("All files", &["*"])
                .pick_file();
            let _ = tx.send(FileResult::Open(path));
        });
    }
    fn poll_files(&mut self, ctx: &egui::Context) {
        let received = self.file_result.as_ref().and_then(|r| match r.try_recv() {
            Ok(v) => Some(Ok(v)),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("File worker stopped unexpectedly.".to_string()))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        });
        let Some(received) = received else { return };
        self.file_result = None;
        let continue_after_save = std::mem::take(&mut self.continue_after_save);
        match received {
            Ok(FileResult::Open(path)) => {
                self.pending_open = path;
                self.status = if self.pending_open.is_some() {
                    "File selected."
                } else {
                    "Open cancelled."
                }
                .into();
            }
            Ok(FileResult::Save(Ok(Some((path, saved))))) => {
                self.session_path = Some(path);
                self.saved = saved;
                self.status = "Session saved.".into();
                if continue_after_save && !self.dirty() {
                    self.proceed(ctx);
                }
            }
            Ok(FileResult::Save(Ok(None))) => self.status = "Save cancelled.".into(),
            Ok(FileResult::Save(Err(error))) | Err(error) => {
                self.status = format!("Could not complete file operation: {error}");
            }
        }
    }
    fn proceed(&mut self, ctx: &egui::Context) {
        if self.close_dialog {
            self.close_dialog = false;
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if let Some(path) = self.pending_open.take() {
            self.load(path);
        }
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let key = |k| ctx.input(|i| i.key_pressed(k));
        let command = ctx.input(|i| i.modifiers.command);
        if command && key(egui::Key::O) && !self.busy() {
            self.pick_open();
        }
        if command && key(egui::Key::S) {
            self.save(ctx.input(|i| i.modifiers.shift));
        }
        if !self.busy() && command && key(egui::Key::Z) {
            self.forget_marker_selection();
            self.drag = None;
            if ctx.input(|i| i.modifiers.shift) {
                if let Some(next) = self.redo.pop() {
                    self.undo.push(self.session.markers.clone());
                    self.session.markers = next;
                }
            } else if let Some(previous) = self.undo.pop() {
                self.redo.push(self.session.markers.clone());
                self.session.markers = previous;
            }
        }
        if !command && self.ready() {
            if key(egui::Key::Space) {
                self.toggle();
            }
            if key(egui::Key::M) {
                self.marker(Kind::Measure);
            }
            if key(egui::Key::B) {
                self.marker(Kind::Beat);
            }
            if key(egui::Key::L) {
                self.session.looping = !self.session.looping;
                self.sync_selection();
            }
            if key(egui::Key::Escape) {
                self.forget_marker_selection();
                self.session.selection = None;
                self.sync_selection();
            }
            if key(egui::Key::ArrowLeft) {
                self.seek(
                    self.position()
                        - if ctx.input(|i| i.modifiers.shift) {
                            5.
                        } else {
                            1.
                        },
                );
            }
            if key(egui::Key::ArrowRight) {
                self.seek(
                    self.position()
                        + if ctx.input(|i| i.modifiers.shift) {
                            5.
                        } else {
                            1.
                        },
                );
            }
            if key(egui::Key::Home) {
                self.seek(0.);
            }
            if key(egui::Key::Questionmark) {
                self.help = !self.help;
            }
            if key(egui::Key::Q) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    fn zoom_at(&mut self, zoom: f64, fraction: f64) {
        if self.duration <= 0. {
            return;
        }
        let fraction = fraction.clamp(0., 1.);
        let anchor = self.offset + self.duration / self.zoom * fraction;
        self.zoom = zoom.clamp(1., MAX_ZOOM);
        let visible = self.duration / self.zoom;
        self.offset = (anchor - visible * fraction).clamp(0., self.duration - visible);
    }
    fn scroll_waveform(&mut self, seconds: f64) {
        let max = (self.duration - self.duration / self.zoom).max(0.);
        self.offset = (self.offset + seconds).clamp(0., max);
    }
    fn fit_selection(&mut self) {
        if let Some((a, b)) = self.session.selection {
            self.zoom = (self.duration / ((b - a) * 1.1)).clamp(1., MAX_ZOOM);
            let visible = self.duration / self.zoom;
            self.offset = ((a + b - visible) * 0.5).clamp(0., self.duration - visible);
        }
    }
    fn zoom_controls(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.ready(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Waveform zoom");
                if ui
                    .add_enabled(self.zoom > 1., egui::Button::new("−"))
                    .on_hover_text("Zoom out")
                    .clicked()
                {
                    self.zoom_at(self.zoom / 2., 0.5);
                }
                let mut zoom = self.zoom;
                if ui
                    .add(
                        egui::Slider::new(&mut zoom, 1.0..=MAX_ZOOM)
                            .logarithmic(true)
                            .suffix("×"),
                    )
                    .changed()
                {
                    self.zoom_at(zoom, 0.5);
                }
                if ui
                    .add_enabled(self.zoom < MAX_ZOOM, egui::Button::new("+"))
                    .on_hover_text("Zoom in")
                    .clicked()
                {
                    self.zoom_at(self.zoom * 2., 0.5);
                }
                if ui.button("Fit song").clicked() {
                    self.zoom_at(1., 0.5);
                }
                if ui
                    .add_enabled(
                        self.session.selection.is_some(),
                        egui::Button::new("Fit selection"),
                    )
                    .clicked()
                {
                    self.fit_selection();
                }
            });
            if self.zoom > 1. {
                let max = self.duration - self.duration / self.zoom;
                ui.label("Scroll waveform · Shift+wheel or swipe horizontally");
                ui.horizontal(|ui| {
                    let step = self.duration / self.zoom * 0.2;
                    if ui
                        .add_enabled(self.offset > 0., egui::Button::new("◀"))
                        .on_hover_text("Scroll left")
                        .clicked()
                    {
                        self.scroll_waveform(-step);
                    }
                    ui.scope(|ui| {
                        ui.spacing_mut().slider_width = (ui.available_width() - 36.).max(40.);
                        ui.add(egui::Slider::new(&mut self.offset, 0.0..=max).show_value(false));
                    });
                    if ui
                        .add_enabled(self.offset < max, egui::Button::new("▶"))
                        .on_hover_text("Scroll right")
                        .clicked()
                    {
                        self.scroll_waveform(step);
                    }
                });
            }
        });
    }
    fn waveform(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 255.),
            Sense::click_and_drag(),
        );
        let plot = Rect::from_min_max(rect.min + Vec2::new(0., 28.), rect.max - Vec2::new(0., 22.));
        if self.ready() && response.hovered() && !ui.input(|i| i.pointer.any_down()) {
            let (scroll, pointer) = ui.input_mut(|i| {
                let scroll = i.raw_scroll_delta;
                i.raw_scroll_delta = Vec2::ZERO;
                i.smooth_scroll_delta = Vec2::ZERO;
                (scroll, i.pointer.hover_pos())
            });
            if scroll.x != 0. {
                self.scroll_waveform(
                    -scroll.x as f64 / plot.width() as f64 * self.duration / self.zoom,
                );
            } else if let Some(pointer) = pointer
                && scroll.y != 0.
            {
                self.zoom_at(
                    self.zoom * (scroll.y as f64 * 0.005).exp(),
                    ((pointer.x - plot.left()) / plot.width()) as f64,
                );
            }
        }
        let visible = self.duration / self.zoom;
        self.offset = self.offset.clamp(0., (self.duration - visible).max(0.));
        let offset = self.offset;
        let duration = self.duration;
        let to_x =
            |t: f64| plot.left() + ((t - offset) / visible.max(f64::EPSILON)) as f32 * plot.width();
        let raw_time = |x: f32| offset + ((x - plot.left()) / plot.width()) as f64 * visible;
        let from_x = |x: f32| raw_time(x).clamp(0., duration);
        let near_handle = |pos: Pos2, selection: Option<(f64, f64)>| -> Option<bool> {
            let (a, b) = selection?;
            if pos.y < plot.top() || pos.y > plot.bottom() {
                return None;
            }
            let left = (pos.x - to_x(a)).abs();
            let right = (pos.x - to_x(b)).abs();
            let grip_y = pos.y >= plot.bottom() - 30.;
            let on_left = left <= 10. || (grip_y && pos.x >= to_x(a) && pos.x <= to_x(a) + 14.);
            let on_right = right <= 10. || (grip_y && pos.x >= to_x(b) - 14. && pos.x <= to_x(b));
            match (on_left, on_right) {
                (true, true) => Some(left <= right),
                (true, false) => Some(true),
                (false, true) => Some(false),
                _ => None,
            }
        };
        let near_marker = |pos: Pos2, markers: &[Marker]| {
            if !plot.contains(pos) {
                return None;
            }
            markers
                .iter()
                .enumerate()
                .filter_map(|(index, marker)| {
                    let x = to_x(marker.time);
                    let dx = pos.x - x;
                    let on_label = pos.y <= plot.top() + 19. && (0.0..=19.0).contains(&dx);
                    (plot.x_range().contains(x) && (dx.abs() <= 6. || on_label))
                        .then_some((index, dx.abs()))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, _)| index)
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0., Color32::from_rgb(13, 18, 24));
        if self.duration <= 0. {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop an audio file here, or choose Open audio",
                egui::FontId::proportional(18.),
                Color32::GRAY,
            );
            return;
        }
        if self.ready() {
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(pos) = ui.input(|i| i.pointer.press_origin())
            {
                let handle = near_handle(pos, self.session.selection);
                if let Some(index) = near_marker(pos, &self.session.markers)
                    && (handle.is_none() || pos.y <= plot.top() + 19.)
                {
                    self.checkpoint();
                    self.drag = Some(WaveDrag::Marker {
                        index,
                        grab_offset: self.session.markers[index].time - raw_time(pos.x),
                    });
                } else {
                    self.forget_marker_selection();
                    self.drag = Some(match handle {
                        Some(true) => WaveDrag::Start {
                            grab_offset: self.session.selection.unwrap().0 - raw_time(pos.x),
                        },
                        Some(false) => WaveDrag::End {
                            grab_offset: self.session.selection.unwrap().1 - raw_time(pos.x),
                        },
                        None => WaveDrag::Select {
                            start: from_x(pos.x),
                        },
                    });
                }
            }
            if (response.dragged() || response.drag_stopped())
                && let (Some(drag), Some(pos)) = (self.drag, response.interact_pointer_pos())
            {
                let time = from_x(pos.x);
                let min_length = 0.03_f64.min(self.duration);
                match drag {
                    WaveDrag::Marker { index, grab_offset } => {
                        let index = self.move_marker(
                            index,
                            (raw_time(pos.x) + grab_offset).clamp(0., duration),
                        );
                        self.drag = Some(WaveDrag::Marker { index, grab_offset });
                    }
                    WaveDrag::Select { start } if (start - time).abs() >= min_length => {
                        self.session.selection = Some((start.min(time), start.max(time)));
                        self.session.looping = true;
                    }
                    WaveDrag::Start { grab_offset } => {
                        if let Some((_, b)) = self.session.selection {
                            self.session.selection = Some((
                                (raw_time(pos.x) + grab_offset).clamp(0., (b - min_length).max(0.)),
                                b,
                            ));
                        }
                    }
                    WaveDrag::End { grab_offset } => {
                        if let Some((a, _)) = self.session.selection {
                            self.session.selection = Some((
                                a,
                                (raw_time(pos.x) + grab_offset)
                                    .clamp((a + min_length).min(self.duration), self.duration),
                            ));
                        }
                    }
                    _ => {}
                }
                self.sync_selection();
            }
            if response.drag_stopped() {
                if matches!(self.drag, Some(WaveDrag::Select { .. }))
                    && let Some((a, _)) = self.session.selection
                {
                    self.seek(a);
                }
                self.drag = None;
            }
            if response.clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                self.seek(from_x(pos.x));
            }
            if response.secondary_clicked() {
                self.forget_marker_selection();
                self.session.selection = None;
                self.sync_selection();
            }
            if self
                .drag
                .is_some_and(|d| !matches!(d, WaveDrag::Select { .. }))
                || response.hover_pos().is_some_and(|pos| {
                    near_handle(pos, self.session.selection).is_some()
                        || near_marker(pos, &self.session.markers).is_some()
                })
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        for i in 0..=8 {
            let time = self.offset + visible * i as f64 / 8.;
            let x = to_x(time);
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(1_f32, Color32::from_gray(36)),
            );
            painter.text(
                Pos2::new(x + 3., rect.top() + 8.),
                egui::Align2::LEFT_TOP,
                clock(time),
                egui::FontId::monospace(11.),
                Color32::GRAY,
            );
        }
        if let Some((a, b)) = self.session.selection {
            let selection = Rect::from_min_max(
                Pos2::new(to_x(a).max(plot.left()), plot.top()),
                Pos2::new(to_x(b).min(plot.right()), plot.bottom()),
            );
            if selection.is_positive() {
                painter.rect_filled(selection, 0., TEAL.gamma_multiply(0.14));
            }
        }
        let columns = plot.width().ceil() as usize;
        for x in 0..columns {
            let a = from_x(plot.left() + x as f32) / self.duration;
            let b = from_x(plot.left() + x as f32 + 1.) / self.duration;
            let first = (a * self.wave.len() as f64) as usize;
            let last = ((b * self.wave.len() as f64).ceil() as usize)
                .max(first + 1)
                .min(self.wave.len());
            let peak = self
                .wave
                .get(first..last)
                .unwrap_or(&[])
                .iter()
                .copied()
                .fold(0f32, f32::max)
                .min(1.);
            let h = (peak * plot.height() * 0.46).max(0.5);
            let px = plot.left() + x as f32;
            painter.line_segment(
                [
                    Pos2::new(px, plot.center().y - h),
                    Pos2::new(px, plot.center().y + h),
                ],
                Stroke::new(1_f32, TEAL),
            );
        }
        let mut measure = 0;
        let mut beat = 0;
        for marker in &self.session.markers {
            let (label, color) = if marker.kind == Kind::Measure {
                measure += 1;
                beat = 0;
                (format!("M{measure}"), GOLD)
            } else {
                beat += 1;
                (format!("b{beat}"), Color32::from_rgb(138, 167, 245))
            };
            let x = to_x(marker.time);
            if plot.x_range().contains(x) {
                painter.line_segment(
                    [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                    Stroke::new(1_f32, color),
                );
                painter.text(
                    Pos2::new(x + 3., plot.top() + 4.),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::monospace(11.),
                    color,
                );
            }
        }
        let x = to_x(self.position());
        if plot.x_range().contains(x) {
            painter.line_segment(
                [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                Stroke::new(2_f32, Color32::WHITE),
            );
        }
        if let Some((a, b)) = self.session.selection {
            for (time, start) in [(a, true), (b, false)] {
                let x = to_x(time);
                if !plot.x_range().contains(x) {
                    continue;
                }
                let color = if matches!(
                    (self.drag, start),
                    (Some(WaveDrag::Start { .. }), true) | (Some(WaveDrag::End { .. }), false)
                ) {
                    GOLD
                } else {
                    TEAL
                };
                painter.line_segment(
                    [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
                    Stroke::new(2_f32, color),
                );
                let grip_x = if start { x } else { x - 14. };
                let grip = Rect::from_min_size(
                    Pos2::new(grip_x, plot.bottom() - 30.),
                    Vec2::new(14., 30.),
                );
                painter.rect_filled(grip, 2., color);
                for dx in [5., 9.] {
                    painter.line_segment(
                        [
                            Pos2::new(grip_x + dx, grip.top() + 8.),
                            Pos2::new(grip_x + dx, grip.bottom() - 8.),
                        ],
                        Stroke::new(1_f32, Color32::from_rgb(13, 18, 24)),
                    );
                }
            }
        }
        painter.text(
            Pos2::new(plot.left() + 8., rect.bottom() - 7.),
            egui::Align2::LEFT_BOTTOM,
            "DRAG TO SELECT   /   DRAG ENDS TO TRIM   /   WHEEL: ZOOM   /   SHIFT+WHEEL: SCROLL",
            egui::FontId::monospace(10.),
            Color32::GRAY,
        );
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(30));
        if self.theme_check.elapsed() > Duration::from_secs(2) {
            self.theme_check = Instant::now();
            if let Some(home) = std::env::var_os("HOME")
                && let Ok(colors) = std::fs::read_to_string(
                    PathBuf::from(home).join(".local/state/omarchy/current/theme/colors.toml"),
                )
                && let Some(accent) = colors.lines().find_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    (key.trim() == "accent").then(|| {
                        value
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string()
                    })
                })
                && let Ok(color) = Color32::from_hex(&accent)
            {
                ctx.style_mut(|s| {
                    s.visuals.selection.bg_fill = color.gamma_multiply(0.35);
                    s.visuals.widgets.active.bg_fill = color.gamma_multiply(0.4);
                });
            }
        }
        while let Ok(error) = self.errors.try_recv() {
            self.status = error;
        }
        let received = self.result.as_ref().and_then(|r| match r.try_recv() {
            Ok(v) => Some(v),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Audio worker stopped unexpectedly.".into()))
            }
            _ => None,
        });
        if let Some(result) = received {
            self.result = None;
            match result {
                Ok((session, buffers, wave, duration)) => {
                    let fresh = self.duration == 0.;
                    self.session = session;
                    self.draft = self.session.effects.clone();
                    self.wave = wave;
                    self.buffers = buffers;
                    self.bypass = false;
                    self.duration = duration;
                    if let Some(e) = &self.engine {
                        let mut t = e.state.lock().unwrap();
                        t.duration = duration;
                        t.set_audio(
                            self.buffers.modified.clone(),
                            self.session.effects.speed,
                            self.resume_position,
                        );
                        t.playing = self.resume;
                    }
                    self.sync_selection();
                    if fresh {
                        self.session_path = self.load_path.take();
                        self.saved = serde_json::to_string(&self.session).unwrap();
                    }
                    self.status = "Ready. Space to play · M for measure · B for beat".into();
                }
                Err(error) => {
                    self.draft = self.session.effects.clone();
                    self.status = error;
                }
            }
        }
        self.poll_files(ctx);
        self.shortcuts(ctx);
        if !self.allow_close
            && ctx.input(|i| i.viewport().close_requested())
            && (self.dirty() || self.busy())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_dialog = true;
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if dropped.is_some() && !self.busy() {
            self.pending_open = dropped;
        }
        if self.pending_open.is_some() && !self.dirty() && !self.busy() {
            let path = self.pending_open.take().unwrap();
            self.load(path);
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(10.);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("OmaScribe").strong().color(TEAL));
                ui.weak(concat!("v", env!("CARGO_PKG_VERSION")));
                ui.label(
                    egui::RichText::new("MUSIC TRANSCRIPTION WORKBENCH")
                        .small()
                        .color(Color32::GRAY),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("?  Shortcuts").clicked() {
                        self.help = !self.help;
                    }
                    if ui
                        .add_enabled(self.ready(), egui::Button::new("Save session"))
                        .clicked()
                    {
                        self.save(false);
                    }
                    if ui
                        .add_enabled(!self.busy(), egui::Button::new("Open audio…"))
                        .clicked()
                    {
                        self.pick_open();
                    }
                });
            });
            ui.add_space(8.);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.busy() {
                    ui.spinner();
                }
                ui.label(&self.status);
                if self.dirty() {
                    ui.label(egui::RichText::new("• unsaved").color(GOLD));
                }
            });
        });
        egui::SidePanel::right("markers")
            .default_width(230.)
            .min_width(200.)
            .show(ctx, |ui| {
                ui.add_space(12.);
                ui.heading("Markers");
                ui.label("Click to jump. Shift-click a second marker to select the passage.");
                ui.add_space(8.);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.ready(), egui::Button::new("M  Measure"))
                        .clicked()
                    {
                        self.marker(Kind::Measure);
                    }
                    if ui
                        .add_enabled(self.ready(), egui::Button::new("B  Beat"))
                        .clicked()
                    {
                        self.marker(Kind::Beat);
                    }
                });
                ui.separator();
                let mut delete = None;
                let mut jump = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut measure = 0;
                    let mut beat = 0;
                    for (index, marker) in self.session.markers.iter().enumerate() {
                        let label = if marker.kind == Kind::Measure {
                            measure += 1;
                            beat = 0;
                            format!("Measure {measure}")
                        } else {
                            beat += 1;
                            format!("Beat {beat}")
                        };
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.ready(),
                                    egui::SelectableLabel::new(
                                        self.marker_anchor == Some(index)
                                            || self.marker_end == Some(index),
                                        format!("{}   {}", clock(marker.time), label),
                                    ),
                                )
                                .clicked()
                            {
                                jump = Some((
                                    index,
                                    ui.input(|i| i.modifiers.shift || i.modifiers.command),
                                ));
                            }
                            if ui
                                .add_enabled(!self.busy(), egui::Button::new("×").small())
                                .on_hover_text("Delete marker")
                                .clicked()
                            {
                                delete = Some(index);
                            }
                        });
                    }
                    if self.session.markers.is_empty() {
                        ui.add_space(16.);
                        ui.label(
                            egui::RichText::new(
                                "Press M on the first beat of a measure, or B on any beat.",
                            )
                            .color(Color32::GRAY),
                        );
                    }
                });
                if let Some((index, extend)) = jump {
                    self.select_marker(index, extend);
                }
                if let Some(index) = delete {
                    self.checkpoint();
                    self.forget_marker_selection();
                    self.drag = None;
                    self.session.markers.remove(index);
                }
            });
        egui::CentralPanel::default().show(ctx, |ui| {
          egui::ScrollArea::vertical().show(ui, |ui| {
            ui.spacing_mut().slider_width = 80.;
            ui.add_space(12.);
            ui.horizontal(|ui| {
                ui.heading(self.session.audio.file_name().and_then(|s| s.to_str()).unwrap_or("Listen closer."));
                if self.duration > 0. { ui.label(format!("{} total", clock(self.duration))); }
            });
            ui.add_space(8.); self.waveform(ui); ui.add_space(6.);
            ui.horizontal(|ui| {
                let playing = self.engine.as_ref().is_some_and(|e| e.state.lock().unwrap().playing);
                if ui.add_enabled(self.ready(), egui::Button::new(if playing { "Ⅱ  Pause" } else { "▶  Play" }).min_size(Vec2::new(95., 36.))).clicked() { self.toggle(); }
                if ui.add_enabled(self.ready(), egui::Button::new("■  Stop")).clicked() { if let Some(e) = &self.engine { e.state.lock().unwrap().playing = false; } self.seek(0.); }
                ui.label(egui::RichText::new(clock(self.position())).monospace().size(20.));
                if ui.add_enabled(self.ready(), egui::Checkbox::new(&mut self.session.looping, "Loop  [L]")).changed() { self.sync_selection(); }
                if let Some((a,b)) = self.session.selection { ui.label(format!("{:.2}–{:.2}s", a,b)); if ui.small_button("Clear").clicked() { self.forget_marker_selection(); self.session.selection = None; self.sync_selection(); } }
            });
            self.zoom_controls(ui);
            ui.horizontal(|ui| {
                if let Some(e) = &self.engine { let mut volume = e.state.lock().unwrap().volume; if ui.add(egui::Slider::new(&mut volume, 0.0..=1.).text("Volume").show_value(false)).changed() { e.state.lock().unwrap().volume = volume; } }
            });
            ui.separator();
            ui.add_enabled_ui(!self.busy(), |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Tempo"); ui.label("Pitch stays the same");
                    ui.add(egui::Slider::new(&mut self.draft.speed, 0.25..=1.5).step_by(0.01).custom_formatter(|v,_| format!("{:.0}%", v*100.)));
                    for (name, value) in [("½",0.5),("¾",0.75),("1×",1.)] { if ui.button(name).clicked() { self.draft.speed = value; } }
                });
                ui.horizontal(|ui| {
                    ui.heading("Equalizer");
                    if ui.button("Flat").clicked() { self.draft.gains = [0.;5]; self.draft.vocal_cut = false; }
                    if ui.button("Bass focus").clicked() { self.draft.gains = [9., 4., -3., -6., -6.]; self.draft.vocal_cut = false; }
                    if ui.button("Vocal reduction").clicked() { self.draft.gains = [0.;5]; self.draft.vocal_cut = true; }
                    if ui.button("Melody focus").clicked() { self.draft.gains = [-6., -3., 4., 3., 0.]; self.draft.vocal_cut = false; }
                });
                ui.horizontal(|ui| {
                    for (i, name) in ["80 Hz", "250 Hz", "1 kHz", "4 kHz", "10 kHz"].iter().enumerate() {
                        ui.vertical(|ui| { ui.set_min_width(90.); ui.label(*name); ui.add(egui::Slider::new(&mut self.draft.gains[i], -12.0..=12.0).step_by(0.5).suffix(" dB")); });
                    }
                });
                ui.horizontal(|ui| {
                    let mut bypass = self.bypass;
                    if ui.add_enabled(self.ready() && self.session.effects != Effects::default(), egui::Checkbox::new(&mut bypass, "Bypass tempo & EQ")).on_hover_text("Listen at normal speed with flat EQ and stereo center reduction off.").changed() {
                        self.toggle_bypass();
                    }
                    ui.checkbox(&mut self.draft.vocal_cut, "Reduce stereo center");
                    if ui.add_enabled(self.ready() && self.draft != self.session.effects, egui::Button::new("Apply tempo & EQ")).clicked() {
                        let mut session = self.session.clone(); session.effects = self.draft.clone(); self.start_render(session, false);
                    }
                });
                ui.label(egui::RichText::new("Vocal reduction cancels centered stereo audio; it can also remove bass or drums. Mono audio will cancel entirely.").small().color(Color32::GRAY));
            });
          });
        });
        if self.help {
            egui::Window::new("Keyboard shortcuts").open(&mut self.help).collapsible(false).show(ctx, |ui| {
                for (key, action) in [("Space", "Play / pause"), ("M / B", "Mark measure / beat"), ("L / Esc", "Toggle loop / clear selection"), ("← / →", "Seek 1 second (Shift: 5 seconds)"), ("Home", "Return to start"), ("Ctrl+O / Ctrl+S", "Open / save session"), ("Ctrl+Shift+S", "Save session as"), ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo marker edits"), ("? / Q", "Shortcuts / quit")] { ui.horizontal(|ui| { ui.monospace(key); ui.label(action); }); }
                ui.separator(); ui.label("Drag on the waveform to select a loop; drag either end to trim it. Scroll over the waveform to zoom around the pointer. Shift+wheel or horizontal trackpad scrolling moves left and right when zoomed in; the scroll bar and arrow buttons do the same. Use + / −, Fit song, or Fit selection below the waveform. Click to seek. Right-click to clear selection.");
            });
        }
        if self.file_result.is_none()
            && ((self.pending_open.is_some() && self.dirty()) || self.close_dialog)
        {
            egui::Window::new("Save your work?")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(if self.busy() {
                        "Audio is still processing. Close OmaScribe?"
                    } else {
                        "Save your markers, loop, and effects before continuing?"
                    });
                    ui.horizontal(|ui| {
                        let mut proceed = false;
                        if ui
                            .add_enabled(self.ready(), egui::Button::new("Save"))
                            .clicked()
                        {
                            self.continue_after_save = self.save(false);
                        }
                        if ui.button("Discard").clicked() {
                            proceed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending_open = None;
                            self.close_dialog = false;
                        }
                        if proceed {
                            self.proceed(ctx);
                        }
                    });
                });
        }
    }
}
fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.);
    format!("{:02}:{:05.2}", (seconds / 60.) as u64, seconds % 60.)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> App {
        let (_, errors) = mpsc::channel();
        App {
            engine: None,
            errors,
            result: None,
            file_result: None,
            continue_after_save: false,
            session: Session::default(),
            saved: String::new(),
            session_path: None,
            wave: vec![0.5; 100],
            duration: 10.,
            draft: Effects::default(),
            status: String::new(),
            zoom: 1.,
            offset: 0.,
            drag: None,
            marker_anchor: None,
            marker_end: None,
            buffers: PlaybackBuffers::default(),
            bypass: false,
            help: false,
            undo: vec![],
            redo: vec![],
            pending_open: None,
            close_dialog: false,
            allow_close: false,
            load_path: None,
            resume: false,
            resume_position: 0.,
            theme_check: Instant::now(),
        }
    }
    #[test]
    fn pending_file_dialog_keeps_frames_running_and_rejects_duplicate_operations() {
        let mut app = fixture();
        let ctx = egui::Context::default();
        let (tx, rx) = mpsc::channel();
        app.file_result = Some(rx);
        for _ in 0..10 {
            let _ = ctx.run(Default::default(), |ctx| {
                app.poll_files(ctx);
                app.shortcuts(ctx);
            });
        }
        assert!(app.busy());
        assert!(!app.save(false));
        app.pick_open();
        tx.send(FileResult::Open(Some("song.wav".into()))).unwrap();
        app.poll_files(&ctx);
        assert!(!app.busy());
        assert_eq!(app.pending_open, Some("song.wav".into()));
    }

    #[test]
    fn background_save_remembers_path_and_preserves_later_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.omascribe");
        let mut app = fixture();
        app.session_path = Some(path.clone());
        assert!(app.save(false));
        // The saved snapshot must not mark subsequent changes as saved.
        app.session.looping = true;
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.busy() && Instant::now() < deadline {
            app.poll_files(&ctx);
            std::thread::yield_now();
        }
        assert!(!app.busy());
        assert_eq!(app.status, "Session saved.");
        assert_eq!(app.session_path, Some(path.clone()));
        assert!(!Session::read(&path).unwrap().looping);
        assert!(app.dirty());
    }

    #[test]
    fn save_as_completion_updates_the_next_save_destination() {
        let mut app = fixture();
        app.session_path = Some("old.omascribe".into());
        let (tx, rx) = mpsc::channel();
        app.file_result = Some(rx);
        tx.send(FileResult::Save(Ok(Some((
            "new.omascribe".into(),
            serde_json::to_string(&app.session).unwrap(),
        )))))
        .unwrap();
        app.poll_files(&egui::Context::default());
        assert_eq!(app.session_path, Some("new.omascribe".into()));
        assert!(!app.dirty());
    }

    #[test]
    fn save_before_close_waits_for_success_and_retains_changes_on_cancel_or_failure() {
        for result in [
            Ok(None),
            Err("disk full".into()),
            Ok(Some((
                "song.omascribe".into(),
                serde_json::to_string(&Session::default()).unwrap(),
            ))),
        ] {
            let success = matches!(result, Ok(Some(_)));
            let mut app = fixture();
            app.close_dialog = true;
            app.continue_after_save = true;
            let (tx, rx) = mpsc::channel();
            app.file_result = Some(rx);
            let ctx = egui::Context::default();
            app.poll_files(&ctx);
            assert!(!app.allow_close);
            tx.send(FileResult::Save(result)).unwrap();
            app.poll_files(&ctx);
            assert_eq!(app.allow_close, success);
            assert_eq!(app.close_dialog, !success);
            assert_eq!(app.dirty(), !success);
            assert!(!app.continue_after_save);
        }
    }

    #[test]
    fn disconnected_file_worker_does_not_leave_app_busy_or_close_it() {
        let mut app = fixture();
        let (tx, rx) = mpsc::channel();
        app.file_result = Some(rx);
        app.close_dialog = true;
        app.continue_after_save = true;
        drop(tx);
        app.poll_files(&egui::Context::default());
        assert!(!app.busy());
        assert!(!app.allow_close);
        assert!(app.close_dialog);
        assert!(app.status.contains("worker stopped"));
    }

    fn wave_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 500.))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.waveform(ui));
            },
        );
    }
    fn pointer_button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn trim(app: &mut App, ctx: &egui::Context, start: Pos2, end: Pos2) {
        wave_frame(app, ctx, vec![]);
        wave_frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(start),
                pointer_button(start, true),
            ],
        );
        wave_frame(app, ctx, vec![egui::Event::PointerMoved(end)]);
        wave_frame(app, ctx, vec![pointer_button(end, false)]);
    }
    fn wave_x(time: f64, zoom: f64, offset: f64) -> f32 {
        8. + ((time - offset) / (10. / zoom)) as f32 * 784.
    }
    #[test]
    fn dragging_a_selected_marker_past_the_other_keeps_the_range_and_marker_order() {
        for kind in [Kind::Beat, Kind::Measure] {
            let mut app = fixture();
            app.zoom = 2.;
            app.offset = 2.;
            app.session.markers = vec![
                Marker {
                    time: 3.,
                    kind: kind.clone(),
                },
                Marker {
                    time: 5.,
                    kind: Kind::Beat,
                },
            ];
            app.select_marker(1, false);
            app.select_marker(0, true);
            assert_eq!(app.session.selection, Some((3., 5.)));
            let original = app.session.markers.clone();
            trim(
                &mut app,
                &egui::Context::default(),
                Pos2::new(wave_x(3., 2., 2.) + 4., 45.),
                Pos2::new(wave_x(6., 2., 2.) + 4., 45.),
            );
            assert_eq!(app.undo, vec![original]);
            assert_eq!((app.marker_anchor, app.marker_end), (Some(0), Some(1)));
            assert_eq!(app.session.markers[1].kind, kind);
            let (a, b) = app.session.selection.unwrap();
            assert_eq!(a, 5.);
            assert!((b - 6.).abs() < 0.001);
            assert_eq!(app.session.markers[1].time, b);
        }
    }
    #[test]
    fn coincident_markers_do_not_create_an_empty_loop() {
        let mut app = fixture();
        app.session.markers = vec![
            Marker {
                time: 2.,
                kind: Kind::Beat
            };
            2
        ];
        app.select_marker(0, false);
        app.select_marker(1, true);
        assert_eq!(app.session.selection, None);
    }
    #[test]
    fn selection_handles_adjust_each_end_without_replacing_selection() {
        for (from, to, expected) in [
            (2., 3., (3., 6.)),
            (2., 1., (1., 6.)),
            (6., 5., (2., 5.)),
            (6., 8., (2., 8.)),
        ] {
            let mut app = fixture();
            let ctx = egui::Context::default();
            app.session.selection = Some((2., 6.));
            trim(
                &mut app,
                &ctx,
                Pos2::new(wave_x(from, 1., 0.), 150.),
                Pos2::new(wave_x(to, 1., 0.), 150.),
            );
            let (a, b) = app.session.selection.unwrap();
            assert!(
                (a - expected.0).abs() < 0.001 && (b - expected.1).abs() < 0.001,
                "{a}..{b}"
            );
            assert!(!app.session.looping, "Trimming preserves the loop toggle");
        }
    }
    #[test]
    fn selection_handles_stay_ordered_and_inside_the_song() {
        for (from, to, expected) in [
            (2., 9., (5.97, 6.)),
            (6., 0., (2., 2.03)),
            (2., -2., (0., 6.)),
            (6., 12., (2., 10.)),
        ] {
            let mut app = fixture();
            let ctx = egui::Context::default();
            app.session.selection = Some((2., 6.));
            trim(
                &mut app,
                &ctx,
                Pos2::new(wave_x(from, 1., 0.), 150.),
                Pos2::new(wave_x(to, 1., 0.), 150.),
            );
            let (a, b) = app.session.selection.unwrap();
            assert!(
                (a - expected.0).abs() < 0.001 && (b - expected.1).abs() < 0.001,
                "{a}..{b}"
            );
        }
    }
    #[test]
    fn handles_work_when_zoomed_and_at_file_edges() {
        let mut app = fixture();
        let ctx = egui::Context::default();
        app.zoom = 4.;
        app.offset = 3.;
        app.session.selection = Some((3.5, 5.));
        trim(
            &mut app,
            &ctx,
            Pos2::new(wave_x(3.5, 4., 3.), 150.),
            Pos2::new(wave_x(4., 4., 3.), 150.),
        );
        let (a, b) = app.session.selection.unwrap();
        assert!((a - 4.).abs() < 0.001 && b == 5.);
        for (from, to, expected) in [(15., 93.4, (1., 10.)), (785., 706.6, (0., 9.))] {
            let mut app = fixture();
            let ctx = egui::Context::default();
            app.session.selection = Some((0., 10.));
            trim(&mut app, &ctx, Pos2::new(from, 225.), Pos2::new(to, 225.));
            let (a, b) = app.session.selection.unwrap();
            assert!(
                (a - expected.0).abs() < 0.001 && (b - expected.1).abs() < 0.001,
                "{a}..{b}"
            );
        }
    }
    #[test]
    fn grabbing_either_side_of_a_boundary_can_still_reach_file_edges() {
        for (boundary, dx, target, expected) in
            [(2., -5., -30., (0., 6.)), (6., 5., 830., (2., 10.))]
        {
            let mut app = fixture();
            let ctx = egui::Context::default();
            app.session.selection = Some((2., 6.));
            trim(
                &mut app,
                &ctx,
                Pos2::new(wave_x(boundary, 1., 0.) + dx, 150.),
                Pos2::new(target, 150.),
            );
            assert_eq!(app.session.selection, Some(expected));
        }
    }
    #[test]
    fn mouse_wheel_zooms_around_the_pointer_and_fit_restores_the_view() {
        let mut app = fixture();
        let ctx = egui::Context::default();
        let pointer = Pos2::new(wave_x(6., 1., 0.), 120.);
        wave_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pointer)]);
        wave_frame(
            &mut app,
            &ctx,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0., 140.),
                modifiers: Default::default(),
            }],
        );
        assert!(app.zoom > 1.);
        assert!((app.offset + 10. / app.zoom * 0.6 - 6.).abs() < 0.001);
        app.session.selection = Some((4., 6.));
        app.fit_selection();
        assert!(app.offset < 4. && app.offset + 10. / app.zoom > 6.);
        app.zoom_at(1., 0.5);
        assert_eq!(app.zoom, 1.);
        assert_eq!(app.offset, 0.);
        app.zoom_at(1000., 0.5);
        assert_eq!(app.zoom, MAX_ZOOM);
        app.zoom_at(0., 0.5);
        assert_eq!(app.zoom, 1.);
        assert_eq!(app.offset, 0.);
    }
    #[test]
    fn horizontal_and_shift_wheel_scroll_without_changing_zoom_or_selection() {
        for shift in [false, true] {
            let mut app = fixture();
            let ctx = egui::Context::default();
            app.zoom = 4.;
            app.offset = 3.;
            app.session.selection = Some((4., 6.));
            let pointer = Pos2::new(400., 120.);
            wave_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pointer)]);
            for (delta, expected) in [(-156.8, 3.5), (156.8, 3.), (-10000., 7.5), (10000., 0.)] {
                wave_frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: if shift {
                            Vec2::new(0., delta)
                        } else {
                            Vec2::new(delta, 0.)
                        },
                        modifiers: if shift {
                            egui::Modifiers::SHIFT
                        } else {
                            egui::Modifiers::NONE
                        },
                    }],
                );
                assert!(
                    (app.offset - expected).abs() < 0.001,
                    "offset {}",
                    app.offset
                );
                assert_eq!(app.zoom, 4.);
                assert_eq!(app.session.selection, Some((4., 6.)));
            }
        }
    }
    #[test]
    fn scrolling_only_applies_over_the_waveform_and_stops_at_full_view() {
        let mut app = fixture();
        let ctx = egui::Context::default();
        app.zoom = 4.;
        app.offset = 3.;
        wave_frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(Pos2::new(400., 400.))],
        );
        wave_frame(
            &mut app,
            &ctx,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(-100., 0.),
                modifiers: Default::default(),
            }],
        );
        assert_eq!(app.offset, 3.);
        app.zoom_at(1., 0.5);
        app.scroll_waveform(100.);
        assert_eq!(app.offset, 0.);
        app.zoom_at(4., 0.5);
        let offset = app.offset;
        app.scroll_waveform(0.5);
        assert_eq!(app.offset, offset + 0.5);
        app.scroll_waveform(-0.5);
        assert_eq!(app.offset, offset);
    }
    #[test]
    fn dragging_waveform_selects_loop_in_either_direction() {
        for (start, end) in [(100., 400.), (400., 100.)] {
            let mut app = fixture();
            let ctx = egui::Context::default();
            let frame = |app: &mut App, events| {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 500.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| app.waveform(ui));
                    },
                );
            };
            frame(&mut app, vec![]);
            let point = |x| Pos2::new(x, 100.);
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(point(start)),
                    egui::Event::PointerButton {
                        pos: point(start),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            frame(&mut app, vec![egui::Event::PointerMoved(point(end))]);
            frame(
                &mut app,
                vec![egui::Event::PointerButton {
                    pos: point(end),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            let (a, b) = app.session.selection.expect("drag creates selection");
            assert!(a > 1. && a < 1.3 && b > 4.9 && b < 5.1, "{a}..{b}");
            assert!(app.session.looping);
        }
    }
    #[test]
    fn marker_shortcuts_and_undo() {
        let ctx = egui::Context::default();
        let mut app = fixture();
        for key in [egui::Key::M, egui::Key::B, egui::Key::Z] {
            let modifiers = if key == egui::Key::Z {
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND
            } else {
                egui::Modifiers::NONE
            };
            let _ = ctx.run(
                egui::RawInput {
                    modifiers,
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                },
                |ctx| app.shortcuts(ctx),
            );
        }
        assert_eq!(app.session.markers.len(), 1);
        assert_eq!(app.session.markers[0].kind, Kind::Measure);
    }
}
