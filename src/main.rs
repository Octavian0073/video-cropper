//! Video Cropper — Rust + egui.
//! Decoding / exporting is done through the `ffmpeg` and `ffprobe` binaries.

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Id, Key, Layout, Rect, RichText,
    Sense, Stroke, TextStyle, TextureHandle, TextureOptions,
};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------- settings
/// Draw our own title bar (with the X button) instead of the OS one.
const CUSTOM_TITLEBAR: bool = true;
const OUT_SUBDIR: &str = "newpoker";

// ------------------------------------------------------------------- theme
const BG: Color32 = Color32::from_rgb(0x0e, 0x10, 0x14);
const PANEL: Color32 = Color32::from_rgb(0x17, 0x1a, 0x21);
const PANEL2: Color32 = Color32::from_rgb(0x21, 0x25, 0x2e);
const INPUT_BG: Color32 = Color32::from_rgb(0x0b, 0x0d, 0x11);
const BORDER: Color32 = Color32::from_rgb(0x2c, 0x32, 0x3d);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xeb, 0xf1);
const MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa3);
const ACCENT: Color32 = Color32::from_rgb(0x36, 0xe0, 0x6f);
const ACCENT_HI: Color32 = Color32::from_rgb(0x5a, 0xf0, 0x8a);
const ACCENT_DIM: Color32 = Color32::from_rgb(0x1d, 0x6e, 0x3c);
const SEL_BG: Color32 = Color32::from_rgb(0x1b, 0x4d, 0x30);
const DANGER: Color32 = Color32::from_rgb(0xe5, 0x48, 0x4d);
const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x13, 0x0b);
const TRACK: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x3a);

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = INPUT_BG; // text boxes: dark background...
    v.faint_bg_color = PANEL2;
    v.window_rounding = 16.0.into();
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.selection.bg_fill = SEL_BG;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.slider_trailing_fill = true;

    // ...and bright text everywhere, so typed text is never light-on-light.
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.bg_fill = PANEL2;
    v.widgets.inactive.weak_bg_fill = PANEL2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.hovered.bg_fill = TRACK;
    v.widgets.hovered.weak_bg_fill = TRACK;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.bg_fill = BORDER;
    v.widgets.active.weak_bg_fill = BORDER;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    let r = egui::Rounding::same(7.0);
    v.widgets.inactive.rounding = r;
    v.widgets.hovered.rounding = r;
    v.widgets.active.rounding = r;
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(13.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    });
}

// ------------------------------------------------------------ small helpers
fn fmt(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor() as u32;
    let s = t - m as f64 * 60.0;
    format!("{m:02}:{s:04.1}")
}

fn is_video(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_lowercase().as_str(),
                "mp4" | "mkv" | "mov" | "avi" | "webm" | "m4v" | "flv" | "wmv" | "mpg" | "mpeg" | "ts"
            )
        })
        .unwrap_or(false)
}

fn pretty_path(p: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = p.strip_prefix(&home) {
            let r = rest.to_string_lossy();
            return if r.is_empty() { "~".into() } else { format!("~/{r}") };
        }
    }
    p.to_string_lossy().to_string()
}

fn tail(s: &str) -> String {
    let v: Vec<char> = s.chars().collect();
    let start = v.len().saturating_sub(700);
    v[start..].iter().collect()
}

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    ToStart,
    ToEnd,
}

/// Round, painter-drawn icon button (no icon fonts needed).
fn icon_button(ui: &mut egui::Ui, size: f32, kind: Icon, primary: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), sense);
    let hov = resp.hovered() && enabled;
    let bg = if primary {
        if !enabled {
            ACCENT_DIM
        } else if hov {
            ACCENT_HI
        } else {
            ACCENT
        }
    } else if hov {
        BORDER
    } else {
        PANEL2
    };
    let fg = if primary {
        ON_ACCENT
    } else if enabled {
        TEXT
    } else {
        MUTED
    };
    let p = ui.painter();
    p.rect_filled(rect, size / 2.0, bg);
    let c = rect.center();
    let s = size * 0.2;
    match kind {
        Icon::Play => {
            let _ = p.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x - 0.6 * s, c.y - 1.1 * s),
                    pos2(c.x - 0.6 * s, c.y + 1.1 * s),
                    pos2(c.x + 1.1 * s, c.y),
                ],
                fg,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            p.rect_filled(
                Rect::from_min_max(pos2(c.x - 0.9 * s, c.y - s), pos2(c.x - 0.25 * s, c.y + s)),
                1.0,
                fg,
            );
            p.rect_filled(
                Rect::from_min_max(pos2(c.x + 0.25 * s, c.y - s), pos2(c.x + 0.9 * s, c.y + s)),
                1.0,
                fg,
            );
        }
        Icon::ToStart => {
            p.rect_filled(
                Rect::from_min_max(pos2(c.x - 1.0 * s, c.y - 0.9 * s), pos2(c.x - 0.6 * s, c.y + 0.9 * s)),
                1.0,
                fg,
            );
            let _ = p.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x + 1.0 * s, c.y - 0.9 * s),
                    pos2(c.x + 1.0 * s, c.y + 0.9 * s),
                    pos2(c.x - 0.4 * s, c.y),
                ],
                fg,
                Stroke::NONE,
            ));
        }
        Icon::ToEnd => {
            p.rect_filled(
                Rect::from_min_max(pos2(c.x + 0.6 * s, c.y - 0.9 * s), pos2(c.x + 1.0 * s, c.y + 0.9 * s)),
                1.0,
                fg,
            );
            let _ = p.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x - 1.0 * s, c.y - 0.9 * s),
                    pos2(c.x - 1.0 * s, c.y + 0.9 * s),
                    pos2(c.x + 0.4 * s, c.y),
                ],
                fg,
                Stroke::NONE,
            ));
        }
    }
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) {
    egui::Frame::none()
        .fill(PANEL)
        .rounding(10.0)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

// ------------------------------------------------------------ ffmpeg layer
#[derive(Clone)]
struct VideoInfo {
    w: u32,
    h: u32,
    fps: f64,
    duration: f64,
}

fn probe(path: &Path) -> Result<VideoInfo, String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,duration:stream_tags=rotate:stream_side_data=rotation:format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffprobe not found ({e}). Install ffmpeg."))?;
    if !out.status.success() {
        return Err(tail(&String::from_utf8_lossy(&out.stderr)));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (mut w, mut h, mut fps, mut dur, mut rot) = (0u32, 0u32, 25.0f64, None::<f64>, 0i32);
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "width" => w = v.trim().parse().unwrap_or(0),
            "height" => h = v.trim().parse().unwrap_or(0),
            "r_frame_rate" => {
                if let Some((a, b)) = v.split_once('/') {
                    let a: f64 = a.trim().parse().unwrap_or(0.0);
                    let b: f64 = b.trim().parse().unwrap_or(1.0);
                    if a > 0.0 && b > 0.0 {
                        fps = a / b;
                    }
                }
            }
            "duration" => {
                if let Ok(d) = v.trim().parse::<f64>() {
                    if dur.is_none() {
                        dur = Some(d);
                    }
                }
            }
            "rotation" | "TAG:rotate" => rot = v.trim().parse::<f64>().unwrap_or(0.0) as i32,
            _ => {}
        }
    }
    if rot.rem_euclid(180) == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    let duration = dur.unwrap_or(0.0);
    if w == 0 || h == 0 || duration <= 0.0 {
        return Err("no readable video stream".into());
    }
    Ok(VideoInfo { w, h, fps, duration })
}

fn preview_size(w: u32, h: u32) -> (u32, u32) {
    let k = (1440.0 / w as f64).min(810.0 / h as f64).min(1.0);
    let pw = (((w as f64 * k) as u32).max(2)) & !1;
    let ph = (((h as f64 * k) as u32).max(2)) & !1;
    (pw.max(2), ph.max(2))
}

struct VFrame {
    gen: u64,
    ts: f64,
    w: u32,
    h: u32,
    rgb: Vec<u8>,
}

struct StillReq {
    gen: u64,
    path: PathBuf,
    t: f64,
    pw: u32,
    ph: u32,
}

fn grab_frame(path: &Path, t: f64, pw: u32, ph: u32) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss"])
        .arg(format!("{t:.3}"))
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-an", "-vf"])
        .arg(format!("scale={pw}:{ph}"))
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.stdout.len() == (pw * ph * 3) as usize {
        Some(out.stdout)
    } else {
        None
    }
}

/// Background worker that decodes single frames for scrubbing.
/// Only the newest request is processed, so dragging stays responsive.
fn spawn_still_worker(ctx: egui::Context) -> (Sender<StillReq>, Receiver<VFrame>) {
    let (tx, rx) = mpsc::channel::<StillReq>();
    let (otx, orx) = mpsc::channel::<VFrame>();
    thread::spawn(move || {
        while let Ok(mut req) = rx.recv() {
            while let Ok(newer) = rx.try_recv() {
                req = newer;
            }
            if let Some(rgb) = grab_frame(&req.path, req.t, req.pw, req.ph) {
                let f = VFrame { gen: req.gen, ts: req.t, w: req.pw, h: req.ph, rgb };
                if otx.send(f).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        }
    });
    (tx, orx)
}

struct Playback {
    rx: Receiver<VFrame>,
    stop: Arc<AtomicBool>,
    pending: Option<VFrame>,
    clock: Option<(Instant, f64)>,
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn spawn_playback(
    ctx: egui::Context,
    gen: u64,
    path: PathBuf,
    from: f64,
    dur: f64,
    pw: u32,
    ph: u32,
    fps: f64,
) -> Playback {
    let (tx, rx) = mpsc::sync_channel::<VFrame>(6);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    thread::spawn(move || {
        let ofps = fps.clamp(1.0, 30.0);
        let spawned = Command::new("ffmpeg")
            .args(["-v", "error", "-ss"])
            .arg(format!("{from:.3}"))
            .arg("-i")
            .arg(&path)
            .arg("-t")
            .arg(format!("{dur:.3}"))
            .arg("-an")
            .arg("-vf")
            .arg(format!("fps={ofps},scale={pw}:{ph}"))
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(_) => return,
        };
        let Some(mut out) = child.stdout.take() else { return };
        let size = (pw * ph * 3) as usize;
        let mut idx = 0u64;
        loop {
            if stop2.load(Ordering::Relaxed) {
                break;
            }
            let mut buf = vec![0u8; size];
            if out.read_exact(&mut buf).is_err() {
                break;
            }
            let ts = from + idx as f64 / ofps;
            idx += 1;
            if tx.send(VFrame { gen, ts, w: pw, h: ph, rgb: buf }).is_err() {
                break;
            }
            ctx.request_repaint();
        }
        let _ = child.kill();
        let _ = child.wait();
    });
    Playback { rx, stop, pending: None, clock: None }
}

enum ExportMsg {
    Progress(f32),
    Done(Result<PathBuf, String>),
}

struct ExportJob {
    rx: Receiver<ExportMsg>,
    progress: f32,
}

// ------------------------------------------------------------ file browser
#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    rel: String, // sub-folder (relative to the searched folder) for search results
}

enum SearchMsg {
    Batch(Vec<Entry>),
    Done,
}

fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut v: Vec<Entry> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        return None;
                    }
                    let path = e.path();
                    let is_dir = path.is_dir();
                    if !is_dir && !is_video(&path) {
                        return None;
                    }
                    Some(Entry { path, name, is_dir, rel: String::new() })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    v
}

/// Breadth-first search (nearest matches first) through sub-folders.
fn search_walk(root: PathBuf, words: Vec<String>, tx: Sender<SearchMsg>, cancel: Arc<AtomicBool>) {
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    queue.push_back((root.clone(), 0));
    let mut batch: Vec<Entry> = Vec::new();
    let mut total = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = ent.path();
            let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir && depth < 6 {
                queue.push_back((path.clone(), depth + 1));
            }
            if !(is_dir || is_video(&path)) {
                continue;
            }
            let lower = name.to_lowercase();
            if words.iter().all(|w| lower.contains(w.as_str())) {
                let rel = dir
                    .strip_prefix(&root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                batch.push(Entry { path, name, is_dir, rel });
                total += 1;
                if batch.len() >= 20 && tx.send(SearchMsg::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
                if total >= 500 {
                    let _ = tx.send(SearchMsg::Batch(batch));
                    let _ = tx.send(SearchMsg::Done);
                    return;
                }
            }
        }
    }
    let _ = tx.send(SearchMsg::Batch(batch));
    let _ = tx.send(SearchMsg::Done);
}

struct Browser {
    open: bool,
    dir: PathBuf,
    query: String,
    entries: Vec<Entry>,
    results: Vec<Entry>,
    searching: bool,
    search_rx: Option<Receiver<SearchMsg>>,
    cancel: Option<Arc<AtomicBool>>,
    selected: usize,
    scroll_sel: bool,
    focus_search: bool,
}

impl Browser {
    fn new() -> Self {
        Self {
            open: false,
            dir: PathBuf::from("/"),
            query: String::new(),
            entries: vec![],
            results: vec![],
            searching: false,
            search_rx: None,
            cancel: None,
            selected: 0,
            scroll_sel: false,
            focus_search: false,
        }
    }

    fn cancel_search(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.search_rx = None;
        self.searching = false;
    }

    /// Always starts in Downloads (falls back to the home folder).
    fn open_downloads(&mut self) {
        let start = dirs::download_dir()
            .filter(|p| p.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        self.open = true;
        self.navigate(start);
        self.focus_search = true;
    }

    fn navigate(&mut self, dir: PathBuf) {
        self.cancel_search();
        self.dir = dir;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        self.entries = list_dir(&self.dir);
    }

    fn dir_name(&self) -> String {
        self.dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".into())
    }

    fn on_query_changed(&mut self) {
        self.cancel_search();
        self.results.clear();
        self.selected = 0;
        self.scroll_sel = true;
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        if words.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_rx = Some(rx);
        self.cancel = Some(cancel.clone());
        self.searching = true;
        let root = self.dir.clone();
        thread::spawn(move || search_walk(root, words, tx, cancel));
    }

    fn poll(&mut self) {
        let mut done = false;
        if let Some(rx) = &self.search_rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    SearchMsg::Batch(v) => self.results.extend(v),
                    SearchMsg::Done => done = true,
                }
            }
        }
        if done {
            self.searching = false;
        }
    }
}

fn places() -> Vec<(&'static str, PathBuf)> {
    let mut v = vec![];
    if let Some(p) = dirs::download_dir() {
        v.push(("Downloads", p));
    }
    if let Some(p) = dirs::video_dir() {
        v.push(("Videos", p));
    }
    if let Some(p) = dirs::desktop_dir() {
        v.push(("Desktop", p));
    }
    if let Some(p) = dirs::document_dir() {
        v.push(("Documents", p));
    }
    if let Some(p) = dirs::home_dir() {
        v.push(("Home", p));
    }
    v.retain(|(_, p)| p.exists());
    v
}

fn draw_x(p: &egui::Painter, c: egui::Pos2, half: f32, col: Color32) {
    p.line_segment([c + vec2(-half, -half), c + vec2(half, half)], Stroke::new(1.6, col));
    p.line_segment([c + vec2(-half, half), c + vec2(half, -half)], Stroke::new(1.6, col));
}

/// Returns the picked video, if any.
fn browser_ui(b: &mut Browser, ui: &mut egui::Ui, enter: bool, up: bool, down: bool) -> Option<PathBuf> {
    let mut picked: Option<PathBuf> = None;
    let mut nav: Option<PathBuf> = None;
    let mut activate: Option<usize> = None;
    ui.set_width(720.0);

    // header
    ui.horizontal(|ui| {
        ui.label(RichText::new("Open video").size(18.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
            if resp.hovered() {
                ui.painter().circle_filled(r.center(), 15.0, DANGER);
            }
            let col = if resp.hovered() { Color32::WHITE } else { MUTED };
            draw_x(ui.painter(), r.center(), 5.5, col);
            if resp.clicked() {
                b.open = false;
            }
        });
    });
    ui.add_space(8.0);

    // search box: dark field, bright text
    let hint = format!("Type to search in {} and its sub-folders…", b.dir_name());
    let te = ui.add(
        egui::TextEdit::singleline(&mut b.query)
            .hint_text(RichText::new(hint).color(MUTED))
            .font(FontId::proportional(15.0))
            .text_color(TEXT)
            .desired_width(f32::INFINITY)
            .margin(vec2(10.0, 8.0)),
    );
    if b.focus_search {
        te.request_focus();
        b.focus_search = false;
    }
    if te.changed() {
        b.on_query_changed();
    }
    ui.add_space(10.0);

    let searching_mode = !b.query.trim().is_empty();
    let list: Vec<Entry> = if searching_mode { b.results.clone() } else { b.entries.clone() };
    if down && !list.is_empty() {
        b.selected = (b.selected + 1).min(list.len() - 1);
        b.scroll_sel = true;
    }
    if up {
        b.selected = b.selected.saturating_sub(1);
        b.scroll_sel = true;
    }
    if b.selected >= list.len() {
        b.selected = list.len().saturating_sub(1);
    }
    if enter && !list.is_empty() {
        activate = Some(b.selected);
    }

    ui.horizontal_top(|ui| {
        // places
        ui.vertical(|ui| {
            ui.set_width(130.0);
            ui.label(RichText::new("PLACES").size(12.0).color(MUTED).strong());
            for (name, path) in places() {
                let sel = b.dir == path;
                let r = ui.add_sized(
                    [130.0, 28.0],
                    egui::SelectableLabel::new(sel, RichText::new(name).size(14.0)),
                );
                if r.clicked() {
                    nav = Some(path.clone());
                }
            }
        });
        ui.add_space(6.0);

        // results
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let parent = b.dir.parent().map(|p| p.to_path_buf());
                if ui.add_enabled(parent.is_some(), egui::Button::new("Up")).clicked() {
                    nav = parent;
                }
                ui.label(RichText::new(pretty_path(&b.dir)).monospace().size(13.0).color(MUTED));
                if b.searching {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .min_scrolled_height(300.0)
                .max_height(300.0)
                .show(ui, |ui| {
                    if list.is_empty() {
                        let msg = if searching_mode {
                            if b.searching { "Searching…" } else { "No matches" }
                        } else {
                            "No videos or folders here"
                        };
                        ui.add_space(12.0);
                        ui.label(RichText::new(msg).color(MUTED).size(15.0));
                    }
                    for (i, e) in list.iter().enumerate() {
                        let icon = if e.is_dir { "📁" } else { "🎬" };
                        let mut job = egui::text::LayoutJob::default();
                        job.append(
                            &format!("{icon}  {}", e.name),
                            8.0,
                            egui::TextFormat {
                                font_id: FontId::proportional(14.0),
                                color: TEXT,
                                ..Default::default()
                            },
                        );
                        if !e.rel.is_empty() {
                            job.append(
                                &format!("      {}", e.rel),
                                0.0,
                                egui::TextFormat {
                                    font_id: FontId::proportional(12.0),
                                    color: MUTED,
                                    ..Default::default()
                                },
                            );
                        }
                        let resp = ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::SelectableLabel::new(i == b.selected, job),
                        );
                        if b.scroll_sel && i == b.selected {
                            resp.scroll_to_me(Some(Align::Center));
                        }
                        if resp.clicked() {
                            b.selected = i;
                        }
                        if resp.double_clicked() {
                            activate = Some(i);
                        }
                    }
                });
        });
    });
    b.scroll_sel = false;

    ui.add_space(6.0);
    ui.label(
        RichText::new("Up/Down to move   ·   Enter to open   ·   double-click a folder to enter   ·   Esc to close")
            .size(12.0)
            .color(MUTED),
    );
    if b.searching {
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }

    if let Some(i) = activate {
        if let Some(e) = list.get(i) {
            if e.is_dir {
                nav = Some(e.path.clone());
            } else {
                picked = Some(e.path.clone());
            }
        }
    }
    if let Some(p) = nav {
        b.navigate(p);
    }
    picked
}

// --------------------------------------------------------------------- app
#[derive(Clone, Copy, PartialEq)]
enum BarDrag {
    Start,
    End,
    Playhead,
}

struct App {
    path: Option<PathBuf>,
    info: Option<VideoInfo>,
    gen: u64,
    pv: (u32, u32),
    start: f64,
    end: f64,
    pos: f64,
    crop: [i32; 4], // left, top, right, bottom (video pixels)
    tex: Option<TextureHandle>,

    still_tx: Sender<StillReq>,
    still_rx: Receiver<VFrame>,
    play: Option<Playback>,
    resume_after_drag: bool,
    bar_drag: Option<BarDrag>,
    crop_drag: u8,

    browser: Browser,
    export: Option<ExportJob>,
    last_output: Option<PathBuf>,
    status: String,
    status_err: bool,
    maximized: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);
        let (still_tx, still_rx) = spawn_still_worker(cc.egui_ctx.clone());
        let ffmpeg_ok = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        Self {
            path: None,
            info: None,
            gen: 0,
            pv: (2, 2),
            start: 0.0,
            end: 0.0,
            pos: 0.0,
            crop: [0; 4],
            tex: None,
            still_tx,
            still_rx,
            play: None,
            resume_after_drag: false,
            bar_drag: None,
            crop_drag: 0,
            browser: Browser::new(),
            export: None,
            last_output: None,
            status: if ffmpeg_ok {
                String::new()
            } else {
                "ffmpeg not found - install it: sudo pacman -S ffmpeg".into()
            },
            status_err: !ffmpeg_ok,
            maximized: false,
        }
    }

    // ---- loading & frames
    fn load_video(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.play = None;
        self.gen += 1;
        self.status.clear();
        self.status_err = false;
        match probe(&path) {
            Ok(info) => {
                self.pv = preview_size(info.w, info.h);
                self.start = 0.0;
                self.end = info.duration;
                self.pos = 0.0;
                self.crop = [0; 4];
                self.tex = None;
                self.info = Some(info);
                self.path = Some(path);
                self.request_still(0.0);
                ctx.request_repaint();
            }
            Err(e) => {
                self.status = format!("Could not open video: {e}");
                self.status_err = true;
            }
        }
    }

    fn request_still(&self, t: f64) {
        if let (Some(path), Some(info)) = (&self.path, &self.info) {
            let _ = self.still_tx.send(StillReq {
                gen: self.gen,
                path: path.clone(),
                t: t.clamp(0.0, (info.duration - 0.05).max(0.0)),
                pw: self.pv.0,
                ph: self.pv.1,
            });
        }
    }

    fn show_frame(&mut self, ctx: &egui::Context, f: &VFrame) {
        let img = egui::ColorImage::from_rgb([f.w as usize, f.h as usize], &f.rgb);
        match &mut self.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("frame", img, TextureOptions::LINEAR)),
        }
    }

    fn poll_stills(&mut self, ctx: &egui::Context) {
        while let Ok(f) = self.still_rx.try_recv() {
            if f.gen == self.gen && self.play.is_none() {
                self.show_frame(ctx, &f);
            }
        }
    }

    // ---- playback
    fn toggle_play(&mut self, ctx: &egui::Context) {
        if self.info.is_none() {
            return;
        }
        if self.play.is_some() {
            self.play = None;
        } else {
            self.start_play(ctx, self.pos);
        }
    }

    fn start_play(&mut self, ctx: &egui::Context, from: f64) {
        let (Some(path), Some(info)) = (self.path.clone(), self.info.clone()) else { return };
        let mut from = from;
        if from >= self.end - 0.05 || from < self.start {
            from = self.start;
        }
        self.pos = from;
        self.play = Some(spawn_playback(
            ctx.clone(),
            self.gen,
            path,
            from,
            (self.end - from).max(0.1),
            self.pv.0,
            self.pv.1,
            info.fps,
        ));
    }

    fn poll_playback(&mut self, ctx: &egui::Context) {
        let Some(mut pb) = self.play.take() else { return };
        let mut ended = false;
        let mut due: Option<VFrame> = None;
        loop {
            let frame = match pb.pending.take() {
                Some(f) => f,
                None => match pb.rx.try_recv() {
                    Ok(f) => f,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                },
            };
            let (t0, ts0) = *pb.clock.get_or_insert((Instant::now(), frame.ts));
            let target = ts0 + t0.elapsed().as_secs_f64();
            if frame.ts <= target {
                due = Some(frame); // keep only the newest due frame
            } else {
                pb.pending = Some(frame);
                break;
            }
        }
        if let Some(f) = due {
            self.pos = f.ts;
            self.show_frame(ctx, &f);
        }
        if ended {
            self.pos = self.end;
        } else {
            ctx.request_repaint_after(Duration::from_millis(8));
            self.play = Some(pb);
        }
    }

    // ---- export
    fn start_export(&mut self, ctx: &egui::Context) {
        let (Some(path), Some(info)) = (self.path.clone(), self.info.clone()) else { return };
        let [l, t, r, b] = self.crop;
        let mut ow = info.w as i32 - l - r;
        let mut oh = info.h as i32 - t - b;
        if ow < 2 || oh < 2 {
            self.status = "The crop removes the entire frame.".into();
            self.status_err = true;
            return;
        }
        ow -= ow % 2;
        oh -= oh % 2;
        let start = self.start;
        let dur = self.end - self.start;
        if dur <= 0.0 {
            self.status = "End must be after start.".into();
            self.status_err = true;
            return;
        }
        let outdir = dirs::home_dir().unwrap_or_default().join(OUT_SUBDIR);
        let _ = std::fs::create_dir_all(&outdir);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "video".into());
        let out = outdir.join(format!("{stem}_cropped.mp4"));

        let (tx, rx) = mpsc::channel::<ExportMsg>();
        self.export = Some(ExportJob { rx, progress: 0.0 });
        self.status.clear();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-y", "-v", "error", "-nostats", "-progress", "pipe:1", "-ss"])
                .arg(format!("{start:.3}"))
                .arg("-i")
                .arg(&path)
                .arg("-t")
                .arg(format!("{dur:.3}"))
                .arg("-vf")
                .arg(format!("crop={ow}:{oh}:{l}:{t}"))
                .args([
                    "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a",
                    "aac", "-b:a", "192k", "-movflags", "+faststart",
                ])
                .arg(&out)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(ExportMsg::Done(Err(e.to_string())));
                    ctx2.request_repaint();
                    return;
                }
            };
            let stderr = child.stderr.take();
            let err_thread = thread::spawn(move || {
                let mut s = String::new();
                if let Some(mut e) = stderr {
                    let _ = e.read_to_string(&mut s);
                }
                s
            });
            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(v) = line.strip_prefix("out_time_us=") {
                        if let Ok(us) = v.trim().parse::<i64>() {
                            let p = (us as f64 / 1e6 / dur).clamp(0.0, 1.0) as f32;
                            let _ = tx.send(ExportMsg::Progress(p));
                            ctx2.request_repaint();
                        }
                    }
                }
            }
            let status = child.wait();
            let err = err_thread.join().unwrap_or_default();
            let res = match status {
                Ok(s) if s.success() => Ok(out.clone()),
                Ok(_) => Err(tail(&err)),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(ExportMsg::Done(res));
            ctx2.request_repaint();
        });
    }

    fn poll_export(&mut self) {
        let mut done = None;
        if let Some(job) = &mut self.export {
            while let Ok(m) = job.rx.try_recv() {
                match m {
                    ExportMsg::Progress(p) => job.progress = p,
                    ExportMsg::Done(r) => done = Some(r),
                }
            }
        }
        if let Some(r) = done {
            self.export = None;
            match r {
                Ok(p) => {
                    self.status = format!("Saved: {}", p.display());
                    self.status_err = false;
                    self.last_output = Some(p);
                }
                Err(e) => {
                    self.status = format!("Export failed: {e}");
                    self.status_err = true;
                }
            }
        }
    }

    // ---- input
    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(p) = dropped {
            self.browser.open = false;
            self.load_video(ctx, p);
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.browser.open || ctx.wants_keyboard_input() {
            return;
        }
        let (esc, space, ik, ok, left, right, shift) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::I),
                i.key_pressed(Key::O),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.modifiers.shift,
            )
        });
        if esc {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let Some(dur) = self.info.as_ref().map(|i| i.duration) else { return };
        if space {
            self.toggle_play(ctx);
        }
        if ik {
            self.start = self.pos.min(self.end - 0.1).max(0.0);
        }
        if ok {
            self.end = self.pos.max(self.start + 0.1).min(dur);
        }
        if left || right {
            let step = if shift { 5.0 } else { 1.0 };
            let d = if left { -step } else { step };
            self.play = None;
            self.pos = (self.pos + d).clamp(self.start, self.end);
            self.request_still(self.pos);
        }
    }

    // ---- UI pieces
    fn title_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("title")
            .exact_height(34.0)
            .frame(egui::Frame::none().fill(PANEL))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let drag = ui.interact(rect, Id::new("title_drag"), Sense::click_and_drag());
                if CUSTOM_TITLEBAR {
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        self.maximized = !self.maximized;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(self.maximized));
                    }
                }
                let cy = rect.center().y;
                {
                    let p = ui.painter();
                    p.circle_filled(pos2(rect.left() + 18.0, cy), 4.5, ACCENT);
                    p.text(
                        pos2(rect.left() + 32.0, cy),
                        Align2::LEFT_CENTER,
                        "Video Cropper",
                        FontId::proportional(14.5),
                        TEXT,
                    );
                    p.text(
                        pos2(rect.left() + 136.0, cy),
                        Align2::LEFT_CENTER,
                        "live preview · trim · crop",
                        FontId::proportional(12.0),
                        MUTED,
                    );
                }

                let close_r = Rect::from_min_size(pos2(rect.right() - 42.0, rect.top()), vec2(42.0, rect.height()));
                let min_r = Rect::from_min_size(pos2(rect.right() - 84.0, rect.top()), vec2(42.0, rect.height()));
                let rc = ui.interact(close_r, Id::new("tb_close"), Sense::click());
                let rm = ui.interact(min_r, Id::new("tb_min"), Sense::click());
                {
                    let p = ui.painter();
                    if rc.hovered() {
                        p.rect_filled(close_r, 0.0, DANGER);
                    }
                    if rm.hovered() && CUSTOM_TITLEBAR {
                        p.rect_filled(min_r, 0.0, PANEL2);
                    }
                    let col = if rc.hovered() { Color32::WHITE } else { MUTED };
                    draw_x(p, close_r.center(), 5.0, col);
                    if CUSTOM_TITLEBAR {
                        let mc = min_r.center();
                        let mcol = if rm.hovered() { TEXT } else { MUTED };
                        p.line_segment([mc + vec2(-6.0, 0.0), mc + vec2(6.0, 0.0)], Stroke::new(1.6, mcol));
                    }
                }
                if rc.hovered() || (rm.hovered() && CUSTOM_TITLEBAR) {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if rc.clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if rm.clicked() && CUSTOM_TITLEBAR {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
            });
    }

    fn controls_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("controls")
            .resizable(false)
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin {
                left: 12.0,
                right: 12.0,
                top: 2.0,
                bottom: 10.0,
            }))
            .show(ctx, |ui| {
                card(ui, |ui| self.player_card(ui, ctx));
                ui.add_space(6.0);
                card(ui, |ui| self.crop_card(ui));
                ui.add_space(6.0);
                self.export_row(ui, ctx);
            });
    }

    fn player_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let enabled = self.info.is_some();
        let playing = self.play.is_some();
        let dur = self.info.as_ref().map(|i| i.duration).unwrap_or(0.0);
        ui.horizontal(|ui| {
            let kind = if playing { Icon::Pause } else { Icon::Play };
            if icon_button(ui, 34.0, kind, true, enabled).clicked() {
                self.toggle_play(ctx);
            }
            ui.add_space(4.0);
            if icon_button(ui, 26.0, Icon::ToStart, false, enabled).clicked() {
                self.play = None;
                self.pos = self.start;
                self.request_still(self.pos);
            }
            if icon_button(ui, 26.0, Icon::ToEnd, false, enabled).clicked() {
                self.play = None;
                self.pos = self.end;
                self.request_still(self.pos - 0.05);
            }
            ui.add_space(10.0);
            if ui.add_enabled(enabled, egui::Button::new("Set start  [I]")).clicked() {
                self.start = self.pos.min(self.end - 0.1).max(0.0);
            }
            if ui.add_enabled(enabled, egui::Button::new("Set end  [O]")).clicked() {
                self.end = self.pos.max(self.start + 0.1).min(dur);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} / {}", fmt(self.pos), fmt(dur)))
                        .monospace()
                        .size(13.0),
                );
            });
        });
        ui.add_space(0.0);
        if enabled {
            self.seek_bar(ui, ctx);
            ui.label(
                RichText::new(format!(
                    "Trim  {}  to  {}   ·   {:.1}s selected     (drag the white line to choose where playback starts; drag the green handles to trim)",
                    fmt(self.start),
                    fmt(self.end),
                    self.end - self.start
                ))
                .size(11.5)
                .color(MUTED),
            );
        } else {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::hover());
            let ty = rect.bottom() - 16.0;
            ui.painter().rect_filled(
                Rect::from_min_max(pos2(rect.left() + 14.0, ty - 4.0), pos2(rect.right() - 14.0, ty + 4.0)),
                4.0,
                TRACK,
            );
        }
    }

    fn seek_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(info) = self.info.clone() else { return };
        let dur = info.duration;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click_and_drag());
        let pad = 12.0;
        let x0 = rect.left() + pad;
        let x1 = rect.right() - pad;
        let ty = rect.bottom() - 16.0;
        let t2x = |t: f64| x0 + (x1 - x0) * ((t / dur) as f32);
        let x2t = |x: f32| ((((x - x0) / (x1 - x0)) as f64) * dur).clamp(0.0, dur);

        let (xs, xe) = (t2x(self.start), t2x(self.end));

        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (ds, de) = ((p.x - xs).abs(), (p.x - xe).abs());
                self.bar_drag = Some(if ds.min(de) <= 10.0 {
                    if ds <= de {
                        BarDrag::Start
                    } else {
                        BarDrag::End
                    }
                } else {
                    BarDrag::Playhead
                });
                self.resume_after_drag = self.play.is_some();
                self.play = None;
            }
        }
        if resp.dragged() {
            if let (Some(kind), Some(p)) = (self.bar_drag, resp.interact_pointer_pos()) {
                let t = x2t(p.x);
                match kind {
                    BarDrag::Start => {
                        self.start = t.min(self.end - 0.1).max(0.0);
                        self.pos = self.start;
                    }
                    BarDrag::End => {
                        self.end = t.max(self.start + 0.1).min(dur);
                        self.pos = (self.end - 0.05).max(self.start);
                    }
                    BarDrag::Playhead => {
                        self.pos = t.clamp(self.start, self.end);
                    }
                }
                self.request_still(self.pos);
            }
        }
        if resp.drag_stopped() {
            self.bar_drag = None;
            if self.resume_after_drag {
                self.resume_after_drag = false;
                self.start_play(ctx, self.pos);
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos().or(resp.hover_pos()) {
                let was_playing = self.play.is_some();
                self.play = None;
                self.pos = x2t(p.x).clamp(self.start, self.end);
                self.request_still(self.pos);
                if was_playing {
                    self.start_play(ctx, self.pos);
                }
            }
        }
        if let Some(h) = resp.hover_pos() {
            let near = (h.x - xs).abs().min((h.x - xe).abs()) <= 10.0;
            ctx.set_cursor_icon(if near { CursorIcon::ResizeHorizontal } else { CursorIcon::PointingHand });
        }

        // paint
        let xp = t2x(self.pos);
        let p = ui.painter();
        let bar = |a: f32, b: f32| Rect::from_min_max(pos2(a, ty - 3.0), pos2(b.max(a), ty + 3.0));
        p.rect_filled(bar(x0, x1), 4.0, TRACK);
        p.rect_filled(bar(xs, xe), 4.0, ACCENT_DIM);
        p.rect_filled(bar(xs, xp.max(xs)), 4.0, ACCENT);
        for hx in [xs, xe] {
            let r = Rect::from_center_size(pos2(hx, ty), vec2(8.0, 24.0));
            p.rect_filled(r, 4.0, ACCENT_HI);
            for dx in [-1.5f32, 1.5] {
                p.line_segment(
                    [pos2(hx + dx, ty - 4.0), pos2(hx + dx, ty + 4.0)],
                    Stroke::new(1.0, ON_ACCENT),
                );
            }
        }
        p.line_segment([pos2(xp, ty - 14.0), pos2(xp, ty + 14.0)], Stroke::new(2.0, Color32::WHITE));
        p.circle_filled(pos2(xp, ty - 14.0), 5.5, Color32::WHITE);
        if let Some(h) = resp.hover_pos() {
            let t = x2t(h.x);
            let r = Rect::from_center_size(pos2(h.x.clamp(x0, x1), ty - 26.0), vec2(56.0, 16.0));
            p.rect_filled(r, 6.0, TRACK);
            p.text(r.center(), Align2::CENTER_CENTER, fmt(t), FontId::monospace(11.5), TEXT);
        }
    }

    fn crop_card(&mut self, ui: &mut egui::Ui) {
        let enabled = self.info.is_some();
        let (w, h) = self
            .info
            .as_ref()
            .map(|i| (i.w as i32, i.h as i32))
            .unwrap_or((0, 0));
        ui.horizontal(|ui| {
            ui.label(RichText::new("CROP").size(12.0).color(MUTED).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(enabled, egui::Button::new("Reset")).clicked() {
                    self.crop = [0; 4];
                }
                if enabled {
                    let ow = w - self.crop[0] - self.crop[2];
                    let oh = h - self.crop[1] - self.crop[3];
                    ui.label(RichText::new(format!("{ow}×{oh} px")).monospace().color(ACCENT));
                }
            });
        });
        ui.spacing_mut().slider_width =
            ((ui.available_width() - 4.0 * 120.0 - 3.0 * 14.0) / 4.0).max(60.0);
        egui::Grid::new("crop_grid")
            .num_columns(8)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                let names = ["Left", "Top", "Right", "Bottom"];
                for i in 0..4 {
                    ui.label(names[i]);
                    let opp = [2usize, 3, 0, 1][i];
                    let total = if i % 2 == 0 { w } else { h };
                    let max = (total - 16 - self.crop[opp]).max(1);
                    let mut v = self.crop[i].min(max);
                    ui.add_enabled(enabled, egui::Slider::new(&mut v, 0..=max).suffix(" px"));
                    self.crop[i] = v;
                    if i == 3 {
                        ui.end_row();
                    }
                }
            });
    }

    fn export_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let exporting = self.export.is_some();
            let btn = egui::Button::new(
                RichText::new("Export cropped video").size(14.0).strong().color(ON_ACCENT),
            )
            .fill(ACCENT)
            .rounding(8.0)
            .min_size(vec2(200.0, 34.0));
            if ui.add_enabled(self.info.is_some() && !exporting, btn).clicked() {
                self.start_export(ctx);
            }
            if let Some(job) = &self.export {
                ui.add(
                    egui::ProgressBar::new(job.progress)
                        .desired_width(240.0)
                        .fill(ACCENT)
                        .show_percentage(),
                );
                ui.label(RichText::new("Exporting…").color(MUTED));
            } else if !self.status.is_empty() {
                let col = if self.status_err { DANGER } else { MUTED };
                ui.label(RichText::new(&self.status).color(col));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(p) = &self.last_output {
                    if ui.button("Show in folder").clicked() {
                        let dir = p.parent().unwrap_or(p.as_path());
                        let _ = Command::new("xdg-open").arg(dir).spawn();
                    }
                }
            });
        });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin {
                left: 12.0,
                right: 12.0,
                top: 6.0,
                bottom: 4.0,
            }))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let open = egui::Button::new(RichText::new("Open video").size(13.5).strong())
                        .min_size(vec2(110.0, 28.0));
                    if ui.add(open).clicked() {
                        self.browser.open_downloads();
                    }
                    if let (Some(p), Some(i)) = (&self.path, &self.info) {
                        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        ui.label(RichText::new(name).strong());
                        ui.label(
                            RichText::new(format!(
                                "{}×{}  ·  {:.1}s  ·  {:.0} fps  ·  silent preview",
                                i.w, i.h, i.duration, i.fps
                            ))
                            .color(MUTED),
                        );
                    } else {
                        ui.label(RichText::new("No video selected").color(MUTED));
                    }
                });
                ui.add_space(4.0);
                self.preview_ui(ui, ctx);
            });
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let size = ui.available_size().max(vec2(200.0, 150.0));
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 10.0, Color32::from_rgb(0x08, 0x09, 0x0c));
        painter.rect_stroke(rect, 10.0, Stroke::new(1.0, BORDER));

        let Some(info) = self.info.clone() else {
            let c = rect.center();
            painter.text(
                c - vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                "Open a video to get started",
                FontId::proportional(22.0),
                TEXT,
            );
            painter.text(
                c + vec2(0.0, 18.0),
                Align2::CENTER_CENTER,
                "or drag and drop a file onto this window",
                FontId::proportional(14.0),
                MUTED,
            );
            return;
        };
        let Some(tex_id) = self.tex.as_ref().map(|t| t.id()) else {
            painter.text(rect.center(), Align2::CENTER_CENTER, "Loading preview…", FontId::proportional(18.0), MUTED);
            return;
        };

        let inner = rect.shrink(3.0);
        let k = (inner.width() / info.w as f32).min(inner.height() / info.h as f32);
        let vr = Rect::from_center_size(inner.center(), vec2(info.w as f32 * k, info.h as f32 * k));
        painter.image(
            tex_id,
            vr,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );

        let sc = vr.width() / info.w as f32;
        let [l, t, r, b] = self.crop;
        let cr = Rect::from_min_max(
            pos2(vr.left() + l as f32 * sc, vr.top() + t as f32 * sc),
            pos2(vr.right() - r as f32 * sc, vr.bottom() - b as f32 * sc),
        );
        let dim = Color32::from_black_alpha(150);
        painter.rect_filled(Rect::from_min_max(vr.min, pos2(vr.right(), cr.top())), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(pos2(vr.left(), cr.bottom()), vr.max), 0.0, dim);
        painter.rect_filled(
            Rect::from_min_max(pos2(vr.left(), cr.top()), pos2(cr.left(), cr.bottom())),
            0.0,
            dim,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(cr.right(), cr.top()), pos2(vr.right(), cr.bottom())),
            0.0,
            dim,
        );
        painter.rect_stroke(cr, 0.0, Stroke::new(2.0, ACCENT));
        let mids = [
            cr.left_top(),
            cr.right_top(),
            cr.left_bottom(),
            cr.right_bottom(),
            cr.center_top(),
            cr.center_bottom(),
            cr.left_center(),
            cr.right_center(),
        ];
        for m in mids {
            painter.rect_filled(Rect::from_center_size(m, vec2(9.0, 9.0)), 2.0, ACCENT);
        }
        let ow = info.w as i32 - l - r;
        let oh = info.h as i32 - t - b;
        painter.text(
            pos2(vr.left() + 11.0, vr.bottom() - 9.0),
            Align2::LEFT_BOTTOM,
            format!("{ow}×{oh}"),
            FontId::monospace(13.0),
            Color32::BLACK,
        );
        painter.text(
            pos2(vr.left() + 10.0, vr.bottom() - 10.0),
            Align2::LEFT_BOTTOM,
            format!("{ow}×{oh}"),
            FontId::monospace(13.0),
            ACCENT,
        );

        // edge / corner dragging
        let mask_at = |pos: egui::Pos2| -> u8 {
            let tol = 12.0;
            let in_y = pos.y >= cr.top() - tol && pos.y <= cr.bottom() + tol;
            let in_x = pos.x >= cr.left() - tol && pos.x <= cr.right() + tol;
            let mut m = 0u8;
            if in_y && (pos.x - cr.left()).abs() <= tol {
                m |= 1;
            }
            if in_x && (pos.y - cr.top()).abs() <= tol {
                m |= 2;
            }
            if in_y && (pos.x - cr.right()).abs() <= tol {
                m |= 4;
            }
            if in_x && (pos.y - cr.bottom()).abs() <= tol {
                m |= 8;
            }
            m
        };
        let cursor_for = |m: u8| match m {
            1 | 4 => CursorIcon::ResizeHorizontal,
            2 | 8 => CursorIcon::ResizeVertical,
            3 | 12 => CursorIcon::ResizeNwSe,
            6 | 9 => CursorIcon::ResizeNeSw,
            _ => CursorIcon::Default,
        };
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                self.crop_drag = mask_at(p);
            }
        }
        if self.crop_drag != 0 {
            ctx.set_cursor_icon(cursor_for(self.crop_drag));
        } else if let Some(h) = resp.hover_pos() {
            let m = mask_at(h);
            if m != 0 {
                ctx.set_cursor_icon(cursor_for(m));
            }
        }
        if resp.dragged() && self.crop_drag != 0 {
            if let Some(p) = resp.interact_pointer_pos() {
                let vx = ((p.x - vr.left()) / sc) as i32;
                let vy = ((p.y - vr.top()) / sc) as i32;
                const MIN: i32 = 16;
                let (w, h) = (info.w as i32, info.h as i32);
                let m = self.crop_drag;
                if m & 1 != 0 {
                    self.crop[0] = vx.clamp(0, (w - self.crop[2] - MIN).max(0));
                }
                if m & 2 != 0 {
                    self.crop[1] = vy.clamp(0, (h - self.crop[3] - MIN).max(0));
                }
                if m & 4 != 0 {
                    self.crop[2] = (w - vx).clamp(0, (w - self.crop[0] - MIN).max(0));
                }
                if m & 8 != 0 {
                    self.crop[3] = (h - vy).clamp(0, (h - self.crop[1] - MIN).max(0));
                }
            }
        }
        if resp.drag_stopped() {
            self.crop_drag = 0;
        }
        if resp.clicked() {
            let on_edge = resp.interact_pointer_pos().map(|p| mask_at(p) != 0).unwrap_or(false);
            if !on_edge {
                self.toggle_play(ctx);
            }
        }
    }

    fn browser_window(&mut self, ctx: &egui::Context) {
        if !self.browser.open {
            return;
        }
        self.browser.poll();
        let (esc, enter, up, down) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
            )
        });
        if esc {
            self.browser.cancel_search();
            self.browser.open = false;
            return;
        }
        let screen = ctx.screen_rect();
        egui::Area::new(Id::new("scrim"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(175));
            });
        let b = &mut self.browser;
        let out = egui::Window::new("Open video")
            .id(Id::new("browser_win"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(PANEL)
                    .rounding(18.0)
                    .inner_margin(20.0)
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(ctx, |ui| browser_ui(b, ui, enter, up, down));
        if let Some(path) = out.and_then(|r| r.inner).flatten() {
            self.browser.cancel_search();
            self.browser.open = false;
            self.load_video(ctx, path);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_stills(ctx);
        self.poll_playback(ctx);
        self.poll_export();
        self.handle_dropped(ctx);
        self.handle_keys(ctx);

        self.title_bar(ctx);
        self.controls_panel(ctx);
        self.central(ctx);
        self.browser_window(ctx);
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([960.0, 700.0])
            .with_decorations(!CUSTOM_TITLEBAR)
            .with_title("Video Cropper"),
        ..Default::default()
    };
    eframe::run_native(
        "Video Cropper",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
