use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_numerics::Vector2 as D2D_POINT_2F;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_PIXEL_FORMAT,
    D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory,
    ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_MEDIUM, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint, HBITMAP, HDC, InvalidateRect, PAINTSTRUCT, SRCCOPY, SelectObject};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE,
    TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey, VK_DOWN, VK_LEFT, VK_OEM_4, VK_OEM_6,
    VK_RIGHT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, GA_ROOT,
    GUITHREADINFO, GetAncestor, GetClassNameW, GetClientRect, GetForegroundWindow,
    GetGUIThreadInfo, GetParent, GetWindowLongPtrW, GetWindowThreadProcessId, IDC_ARROW,
    IDC_HAND, KillTimer, LoadCursorW, MoveWindow, RegisterClassW, SW_SHOWNA, SetCursor,
    SetTimer, SetWindowLongPtrW, ShowWindow, WINDOW_EX_STYLE, WINDOW_LONG_PTR_INDEX, WM_DESTROY,
    WM_ERASEBKGND, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SETTINGCHANGE,
    WM_SIZE, WM_TIMER, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::audio::{
    AudioMeta, AudioPlayer, CacheKey, WM_APP_AUDIO_META, WM_APP_PLAY_STATE, WM_APP_WAVEFORM_READY,
    WaveformData, WaveformResultSlot, spawn_waveform_scan,
};
use crate::settings::{SPEEDS, Theme, UserSettings, detect_theme, load_settings, log_msg, save_settings};
use crate::video::{VideoPlayer, WM_APP_VIDEO_EVENT};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_PRINTCLIENT: u32 = 0x0318;
const TIMER_RENDER: usize = 1;
const TIMER_HOTKEY: usize = 2;
const HOTKEY_SPACE_ID: i32 = 0x51A0;
const GWLP_USERDATA_IDX: WINDOW_LONG_PTR_INDEX = WINDOW_LONG_PTR_INDEX(-21);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Video,
}

pub struct MediaInput {
    pub file_path: Option<String>,
    pub file_name: String,
    pub ext: String,
    pub file_size: u64,
    pub mtime_ns: u128,
    pub mem_data: Option<Arc<Vec<u8>>>,
    pub kind: MediaKind,
}

pub fn classify_extension(ext: &str) -> MediaKind {
    match ext.to_ascii_lowercase().as_str() {
        ".mp4" | ".m4v" | ".webm" | ".mov" | ".ogv" | ".mkv" | ".wmv" | ".avi" => MediaKind::Video,
        _ => MediaKind::Audio,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HitTarget {
    None,
    WaveOrScrub,
    VideoCanvas,
    PlayBtn,
    LoopBtn,
    SpeedCompact,
    SpeedSeg(usize),
    MuteBtn,
    VolumeTrack,
    AutoPlayBtn,
}

#[derive(Clone, Copy, Default)]
struct Layout {
    width: f32,
    height: f32,
    header_rect: D2D_RECT_F,
    name_rect: D2D_RECT_F,
    meta_rect: D2D_RECT_F,
    divider_y: f32,
    video_rect: D2D_RECT_F,
    wave_rect: D2D_RECT_F,
    ctrl_rect: D2D_RECT_F,
    play_btn: D2D_RECT_F,
    time_rect: D2D_RECT_F,
    loop_btn: D2D_RECT_F,
    speed_inline: bool,
    speed_rect: D2D_RECT_F,
    speed_segs: [D2D_RECT_F; 6],
    mute_btn: D2D_RECT_F,
    vol_rect: D2D_RECT_F,
    auto_btn: D2D_RECT_F,
}

fn rect_f(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn contains(r: D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

fn format_time(sec: f64, total_sec: f64) -> String {
    let s = sec.max(0.0);
    if total_sec >= 3600.0 {
        let total = s.floor() as u64;
        let h = total / 3600;
        let m = (total % 3600) / 60;
        let rem = total % 60;
        format!("{h}:{m:02}:{rem:02}")
    } else if total_sec > 0.0 && total_sec < 10.0 {
        let m = (s / 60.0).floor() as u64;
        let rem = s - (m as f64 * 60.0);
        format!("{m:02}:{rem:04.1}")
    } else {
        let total = s.floor() as u64;
        let m = total / 60;
        let rem = total % 60;
        format!("{m:02}:{rem:02}")
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_speed(s: f32) -> &'static str {
    if (s - 0.5).abs() < 0.01 {
        "0.5×"
    } else if (s - 0.75).abs() < 0.01 {
        "0.75×"
    } else if (s - 1.0).abs() < 0.01 {
        "1×"
    } else if (s - 1.25).abs() < 0.01 {
        "1.25×"
    } else if (s - 1.5).abs() < 0.01 {
        "1.5×"
    } else {
        "2×"
    }
}

struct D2DResources {
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    stroke_round: ID2D1StrokeStyle,
    fmt_title: IDWriteTextFormat,
    fmt_meta: IDWriteTextFormat,
    fmt_mono: IDWriteTextFormat,
    fmt_small_center: IDWriteTextFormat,
    fmt_tip: IDWriteTextFormat,
    mem_dc: HDC,
    mem_bmp: HBITMAP,
    buf_w: i32,
    buf_h: i32,
}

impl Drop for D2DResources {
    fn drop(&mut self) {
        unsafe {
            if !self.mem_bmp.0.is_null() {
                let _ = DeleteObject(self.mem_bmp.into());
            }
            if !self.mem_dc.0.is_null() {
                let _ = DeleteDC(self.mem_dc);
            }
        }
    }
}

pub struct PreviewWindow {
    hwnd: HWND,
    parent_hwnd: HWND,
    dpi: f32,
    theme: Theme,
    settings: UserSettings,
    d2d_factory: ID2D1Factory,
    dwrite_factory: IDWriteFactory,
    res: Option<D2DResources>,

    // Current media
    kind: MediaKind,
    file_name: String,
    ext_upper: String,
    file_size: u64,
    audio_meta: Option<AudioMeta>,
    waveform: Option<Arc<WaveformData>>,
    waveform_slot: Option<Arc<WaveformResultSlot>>,
    waveform_cancel: Option<Arc<AtomicBool>>,
    audio_player: Option<AudioPlayer>,
    video_player: Option<VideoPlayer>,
    error_msg: Option<String>,

    // Interaction state
    layout: Layout,
    hover_target: HitTarget,
    hover_wave_ratio: Option<f32>,
    mouse_tracking: bool,
    dragging_wave: bool,
    dragging_vol: bool,
    hotkey_registered: bool,
    render_timer_on: bool,
    video_warmup_ticks: u32,
}

fn register_window_class() -> PCWSTR {
    static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();
    let cls = w!("MediaPreviewPaneNativeWnd");
    CLASS_REGISTERED.get_or_init(|| unsafe {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: cls,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
    });
    cls
}

impl PreviewWindow {
    pub fn create(parent: HWND, rc: RECT, input: MediaInput) -> Result<Box<Self>, String> {
        let cls = register_window_class();

        let d2d_factory: ID2D1Factory = unsafe {
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("D2D1CreateFactory: {e}"))?
        };
        let dwrite_factory: IDWriteFactory = unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                .map_err(|e| format!("DWriteCreateFactory: {e}"))?
        };

        let mut w = rc.right - rc.left;
        let mut h = rc.bottom - rc.top;
        if w <= 0 || h <= 0 {
            let mut prc = RECT::default();
            if unsafe { GetClientRect(parent, &mut prc) }.is_ok() {
                w = (prc.right - prc.left).max(1);
                h = (prc.bottom - prc.top).max(1);
            } else {
                w = w.max(1);
                h = h.max(1);
            }
        }

        let settings = load_settings();
        let theme = detect_theme();

        let mut boxed = Box::new(Self {
            hwnd: HWND::default(),
            parent_hwnd: parent,
            dpi: 96.0,
            theme,
            settings,
            d2d_factory,
            dwrite_factory,
            res: None,
            kind: input.kind,
            file_name: input.file_name.clone(),
            ext_upper: input.ext.trim_start_matches('.').to_ascii_uppercase(),
            file_size: input.file_size,
            audio_meta: None,
            waveform: None,
            waveform_slot: None,
            waveform_cancel: None,
            audio_player: None,
            video_player: None,
            error_msg: None,
            layout: Layout::default(),
            hover_target: HitTarget::None,
            hover_wave_ratio: None,
            mouse_tracking: false,
            dragging_wave: false,
            dragging_vol: false,
            hotkey_registered: false,
            render_timer_on: false,
            video_warmup_ticks: 0,
        });

        let raw_ptr = (&mut *boxed as *mut PreviewWindow) as isize;
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                cls,
                w!("MediaPreviewPane"),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
                rc.left,
                rc.top,
                w,
                h,
                Some(parent),
                None,
                None,
                None,
            )
            .map_err(|e| format!("CreateWindowExW: {e}"))?
        };

        boxed.hwnd = hwnd;
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA_IDX, raw_ptr);
            let dpi = GetDpiForWindow(hwnd);
            if dpi > 0 {
                boxed.dpi = dpi as f32;
            }
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            SetTimer(Some(hwnd), TIMER_HOTKEY, 120, None);
        }

        boxed.load_media(input);
        boxed.recompute_layout();
        boxed.sync_hotkey();
        boxed.invalidate();

        Ok(boxed)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn resize(&mut self, rc: RECT) {
        let mut w = rc.right - rc.left;
        let mut h = rc.bottom - rc.top;
        if w <= 0 || h <= 0 {
            let mut prc = RECT::default();
            if unsafe { GetClientRect(self.parent_hwnd, &mut prc) }.is_ok() {
                w = (prc.right - prc.left).max(1);
                h = (prc.bottom - prc.top).max(1);
            }
        }
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = MoveWindow(self.hwnd, rc.left, rc.top, w.max(1), h.max(1), true);
            }
        }
    }

    pub fn focus(&self) {
        if !self.hwnd.0.is_null() {
            unsafe { let _ = SetFocus(Some(self.hwnd)); }
        }
    }

    pub fn destroy_window(&mut self) {
        self.stop_media();
        self.unregister_hotkey();
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_RENDER);
                let _ = KillTimer(Some(self.hwnd), TIMER_HOTKEY);
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA_IDX, 0);
                let _ = DestroyWindow(self.hwnd);
            }
            self.hwnd = HWND::default();
        }
        self.res = None;
    }

    fn stop_media(&mut self) {
        if let Some(c) = self.waveform_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.waveform_slot = None;
        if let Some(mut ap) = self.audio_player.take() {
            ap.stop();
        }
        if let Some(mut vp) = self.video_player.take() {
            vp.shutdown();
        }
        self.set_render_timer(false);
    }

    fn load_media(&mut self, input: MediaInput) {
        self.stop_media();
        self.kind = input.kind;
        self.file_name = input.file_name;
        self.ext_upper = input.ext.trim_start_matches('.').to_ascii_uppercase();
        self.file_size = input.file_size;
        self.audio_meta = None;
        self.waveform = None;
        self.error_msg = None;

        let hwnd_usize = self.hwnd.0 as usize;

        match input.kind {
            MediaKind::Audio => {
                let Some(bytes) = input.mem_data else {
                    self.error_msg = Some("无法读取音频文件".to_string());
                    return;
                };
                let cache_key = input.file_path.as_ref().map(|p| CacheKey {
                    id: p.clone(),
                    size: input.file_size,
                    mtime_ns: input.mtime_ns,
                });

                let slot = Arc::new(WaveformResultSlot::new());
                let cancel = Arc::new(AtomicBool::new(false));
                self.waveform_slot = Some(slot.clone());
                self.waveform_cancel = Some(cancel.clone());

                let mut player = AudioPlayer::new(
                    bytes.clone(),
                    input.ext.clone(),
                    0.0,
                    self.settings.loop_enabled,
                    self.settings.muted,
                    self.settings.volume,
                    self.settings.speed,
                    hwnd_usize,
                );

                spawn_waveform_scan(bytes, input.ext, cache_key, slot.clone(), cancel, hwnd_usize);

                // If waveform came straight from cache, grab it synchronously for 0-frame render!
                if let Ok(w) = slot.waveform.lock() {
                    if let Some(wf) = w.clone() {
                        player.set_duration(wf.meta.duration_sec);
                        self.audio_meta = Some(wf.meta.clone());
                        self.waveform = Some(wf);
                    }
                }

                if self.settings.auto_play {
                    player.play();
                    self.set_render_timer(true);
                }
                self.audio_player = Some(player);
            }
            MediaKind::Video => {
                // Start native Media Foundation video player
                let mem_for_wave = if input.file_size <= 24 * 1024 * 1024 {
                    input.mem_data.clone()
                } else {
                    None
                };

                match VideoPlayer::new(
                    input.file_path.as_deref(),
                    input.mem_data,
                    &input.ext,
                    self.settings.loop_enabled,
                    self.settings.muted,
                    self.settings.volume,
                    self.settings.speed,
                    self.settings.auto_play,
                    hwnd_usize,
                ) {
                    Ok(vp) => {
                        self.video_player = Some(vp);
                        self.video_warmup_ticks = 30;
                        self.set_render_timer(true);
                    }
                    Err(e) => {
                        log_msg(&format!("Video init error: {e}"));
                        self.error_msg = Some("该视频编码暂不被系统 Media Foundation 支持，请双击用外部播放器打开".to_string());
                    }
                }

                // Optionally scan audio waveform for short videos (<= 24 MB) in background
                if let Some(bytes) = mem_for_wave {
                    let cache_key = input.file_path.as_ref().map(|p| CacheKey {
                        id: p.clone(),
                        size: input.file_size,
                        mtime_ns: input.mtime_ns,
                    });
                    let slot = Arc::new(WaveformResultSlot::new());
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.waveform_slot = Some(slot.clone());
                    self.waveform_cancel = Some(cancel.clone());
                    spawn_waveform_scan(bytes, input.ext, cache_key, slot.clone(), cancel, hwnd_usize);
                    if let Ok(w) = slot.waveform.lock() {
                        if let Some(wf) = w.clone() {
                            self.waveform = Some(wf);
                        }
                    }
                }
            }
        }
    }

    fn set_render_timer(&mut self, enable: bool) {
        if self.hwnd.0.is_null() {
            return;
        }
        if enable && !self.render_timer_on {
            unsafe { SetTimer(Some(self.hwnd), TIMER_RENDER, 16, None); }
            self.render_timer_on = true;
        } else if !enable && self.render_timer_on {
            unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_RENDER); }
            self.render_timer_on = false;
        }
    }

    fn invalidate(&self) {
        if !self.hwnd.0.is_null() {
            unsafe { let _ = InvalidateRect(Some(self.hwnd), None, false); }
        }
    }

    fn is_playing(&self) -> bool {
        if let Some(ap) = &self.audio_player {
            ap.is_playing()
        } else if let Some(vp) = &self.video_player {
            vp.is_playing()
        } else {
            false
        }
    }

    fn position(&self) -> f64 {
        if let Some(ap) = &self.audio_player {
            ap.position()
        } else if let Some(vp) = &self.video_player {
            vp.position()
        } else {
            0.0
        }
    }

    fn duration(&self) -> f64 {
        if let Some(ap) = &self.audio_player {
            let d = ap.duration();
            if d > 0.0 {
                d
            } else {
                self.audio_meta.as_ref().map(|m| m.duration_sec).unwrap_or(0.0)
            }
        } else if let Some(vp) = &self.video_player {
            vp.duration()
        } else {
            0.0
        }
    }

    fn toggle_play(&mut self) {
        if let Some(ap) = &mut self.audio_player {
            ap.toggle_play();
            let playing = ap.is_playing();
            self.set_render_timer(playing);
        } else if let Some(vp) = &mut self.video_player {
            vp.toggle_play();
            self.video_warmup_ticks = 10;
            self.set_render_timer(true);
        }
        self.invalidate();
    }

    fn seek_to_ratio(&mut self, ratio: f32) {
        let dur = self.duration();
        if dur <= 0.0 {
            return;
        }
        let target = (ratio.clamp(0.0, 1.0) as f64) * dur;
        if let Some(ap) = &mut self.audio_player {
            ap.seek(target);
        } else if let Some(vp) = &mut self.video_player {
            vp.seek(target);
            self.video_warmup_ticks = 12;
            self.set_render_timer(true);
        }
        self.invalidate();
    }

    fn seek_relative(&mut self, delta_sec: f64) {
        let dur = self.duration();
        if dur <= 0.0 {
            return;
        }
        let next = (self.position() + delta_sec).clamp(0.0, dur);
        self.seek_to_ratio((next / dur) as f32);
    }

    fn apply_settings(&mut self) {
        if let Some(ap) = &self.audio_player {
            ap.set_loop(self.settings.loop_enabled);
            ap.set_muted(self.settings.muted);
            ap.set_volume(self.settings.volume);
            ap.set_speed(self.settings.speed);
        }
        if let Some(vp) = &self.video_player {
            vp.set_loop(self.settings.loop_enabled);
            vp.set_muted(self.settings.muted);
            vp.set_volume(self.settings.volume);
            vp.set_speed(self.settings.speed);
        }
        save_settings(&self.settings);
        self.invalidate();
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        self.destroy_window();
    }
}

pub fn file_name_from_path(path: &str) -> (String, String) {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let ext = p
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default();
    (name, ext)
}
impl PreviewWindow {
    fn client_dips(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        if !self.hwnd.0.is_null() {
            unsafe { let _ = GetClientRect(self.hwnd, &mut rc); }
        }
        let scale = (self.dpi / 96.0).max(0.5);
        let w = ((rc.right - rc.left).max(1) as f32) / scale;
        let h = ((rc.bottom - rc.top).max(1) as f32) / scale;
        (w, h)
    }

    fn recompute_layout(&mut self) {
        let (w, h) = self.client_dips();
        let pad_x = 10.0f32;
        let pad_y = 8.0f32;
        let inner_w = (w - pad_x * 2.0).max(120.0);

        let header_h = 22.0f32;
        let header_rect = rect_f(pad_x, pad_y, pad_x + inner_w, pad_y + header_h);
        let meta_w = (inner_w * 0.44).clamp(90.0, 220.0);
        let name_rect = rect_f(
            pad_x + 12.0,
            pad_y,
            (pad_x + inner_w - meta_w - 8.0).max(pad_x + 40.0),
            pad_y + header_h,
        );
        let meta_rect = rect_f(name_rect.right + 6.0, pad_y, pad_x + inner_w, pad_y + header_h);
        let divider_y = header_rect.bottom + 5.0;

        let ctrl_h = 30.0f32;
        let mut video_rect = rect_f(0.0, 0.0, 0.0, 0.0);
        let wave_rect;
        let ctrl_top;

        match self.kind {
            MediaKind::Audio => {
                let avail_h = (h - divider_y - pad_y - ctrl_h - 16.0).max(56.0);
                let wave_h = (avail_h * 0.42).clamp(64.0, 176.0).min(avail_h);
                let wt = divider_y + 8.0;
                wave_rect = rect_f(pad_x, wt, pad_x + inner_w, wt + wave_h);
                ctrl_top = wave_rect.bottom + 7.0;
            }
            MediaKind::Video => {
                let scrub_h = 24.0f32;
                let max_vid_h = (h - divider_y - pad_y - scrub_h - ctrl_h - 22.0).max(80.0);
                let vt = divider_y + 7.0;
                let vid_h = if let Some(vp) = &self.video_player {
                    let (nw, nh) = vp.native_size();
                    if nw > 0 && nh > 0 {
                        (inner_w * (nh as f32 / nw as f32)).clamp(120.0, max_vid_h)
                    } else {
                        (inner_w * 9.0 / 16.0).clamp(120.0, max_vid_h)
                    }
                } else {
                    (inner_w * 9.0 / 16.0).clamp(120.0, max_vid_h)
                };
                video_rect = rect_f(pad_x, vt, pad_x + inner_w, vt + vid_h);
                let wt = video_rect.bottom + 6.0;
                wave_rect = rect_f(pad_x, wt, pad_x + inner_w, wt + scrub_h);
                ctrl_top = wave_rect.bottom + 6.0;
            }
        }

        let ctrl_rect = rect_f(pad_x, ctrl_top, pad_x + inner_w, ctrl_top + ctrl_h);
        let cy = (ctrl_rect.top + ctrl_rect.bottom) * 0.5;

        let mut lx = ctrl_rect.left;
        let play_btn = rect_f(lx, cy - 12.0, lx + 26.0, cy + 12.0);
        lx = play_btn.right + 6.0;

        let time_w = if inner_w < 300.0 { 74.0 } else { 88.0 };
        let time_rect = rect_f(lx, cy - 11.0, lx + time_w, cy + 11.0);
        lx = time_rect.right + 4.0;

        let loop_btn = rect_f(lx, cy - 12.0, lx + 24.0, cy + 12.0);
        lx = loop_btn.right + 6.0;

        let mut rx = ctrl_rect.right;
        let auto_btn = rect_f(rx - 36.0, cy - 11.0, rx, cy + 11.0);
        rx = auto_btn.left - 6.0;

        let show_vol_bar = inner_w >= 320.0;
        let vol_rect = if show_vol_bar {
            let vw = if inner_w >= 460.0 { 54.0 } else { 40.0 };
            let r = rect_f(rx - vw, cy - 10.0, rx, cy + 10.0);
            rx = r.left - 4.0;
            r
        } else {
            rect_f(0.0, 0.0, 0.0, 0.0)
        };

        let mute_btn = rect_f(rx - 22.0, cy - 12.0, rx, cy + 12.0);
        rx = mute_btn.left - 6.0;

        let mid_avail = (rx - lx).max(0.0);
        let speed_inline = mid_avail >= 176.0;
        let mut speed_segs = [rect_f(0.0, 0.0, 0.0, 0.0); 6];
        let speed_rect = if speed_inline {
            let total_sw = mid_avail.min(204.0);
            let seg_w = total_sw / 6.0;
            let sx = lx;
            for i in 0..6 {
                speed_segs[i] = rect_f(
                    sx + i as f32 * seg_w,
                    cy - 10.5,
                    sx + (i + 1) as f32 * seg_w,
                    cy + 10.5,
                );
            }
            rect_f(sx, cy - 10.5, sx + total_sw, cy + 10.5)
        } else if mid_avail >= 34.0 {
            rect_f(lx, cy - 10.5, (lx + 38.0).min(rx), cy + 10.5)
        } else {
            rect_f(0.0, 0.0, 0.0, 0.0)
        };

        self.layout = Layout {
            width: w,
            height: h,
            header_rect,
            name_rect,
            meta_rect,
            divider_y,
            video_rect,
            wave_rect,
            ctrl_rect,
            play_btn,
            time_rect,
            loop_btn,
            speed_inline,
            speed_rect,
            speed_segs,
            mute_btn,
            vol_rect,
            auto_btn,
        };
    }

    fn ensure_d2d(&mut self) -> bool {
        if self.res.is_some() {
            return true;
        }
        if self.hwnd.0.is_null() {
            return false;
        }
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: self.dpi,
            dpiY: self.dpi,
            usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
            minLevel: windows::Win32::Graphics::Direct2D::D2D1_FEATURE_LEVEL_DEFAULT,
        };

        let rt = match unsafe { self.d2d_factory.CreateDCRenderTarget(&rt_props) } {
            Ok(r) => r,
            Err(e) => { log_msg(&format!("CreateDCRenderTarget failed: {e}")); return false; }
        };
        let brush = match unsafe { rt.CreateSolidColorBrush(&self.theme.text_primary, None) } {
            Ok(b) => b,
            Err(e) => { log_msg(&format!("CreateSolidColorBrush failed: {e}")); return false; }
        };

        let stroke_props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            ..Default::default()
        };
        let stroke_round = match unsafe { self.d2d_factory.CreateStrokeStyle(&stroke_props, None) } { Ok(s) => s, Err(e) => { log_msg(&format!("stroke fail: {e}")); return false; } };

        let make_fmt = |family: PCWSTR, weight, size: f32, align| -> Option<IDWriteTextFormat> {
            let f = unsafe {
                self.dwrite_factory
                    .CreateTextFormat(
                        family,
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size,
                        w!("zh-CN"),
                    )
                    .ok()?
            };
            unsafe {
                let _ = f.SetTextAlignment(align);
                let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                let trim = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                if let Ok(ellipsis) = self.dwrite_factory.CreateEllipsisTrimmingSign(&f) {
                    let _ = f.SetTrimming(&trim, &ellipsis);
                }
            }
            Some(f)
        };

        let Some(fmt_title) = make_fmt(w!("Segoe UI"), DWRITE_FONT_WEIGHT_SEMI_BOLD, 12.5, DWRITE_TEXT_ALIGNMENT_LEADING) else { return false; };
        let Some(fmt_meta) = make_fmt(w!("Segoe UI"), DWRITE_FONT_WEIGHT_NORMAL, 10.5, DWRITE_TEXT_ALIGNMENT_TRAILING) else { return false; };
        let Some(fmt_mono) = make_fmt(w!("Consolas"), DWRITE_FONT_WEIGHT_NORMAL, 11.0, DWRITE_TEXT_ALIGNMENT_LEADING) else { return false; };
        let Some(fmt_small_center) = make_fmt(w!("Segoe UI"), DWRITE_FONT_WEIGHT_MEDIUM, 10.5, DWRITE_TEXT_ALIGNMENT_CENTER) else { return false; };
        let Some(fmt_tip) = make_fmt(w!("Consolas"), DWRITE_FONT_WEIGHT_NORMAL, 10.0, DWRITE_TEXT_ALIGNMENT_CENTER) else { return false; };

        self.res = Some(D2DResources {
            rt,
            brush,
            stroke_round,
            fmt_title,
            fmt_meta,
            fmt_mono,
            fmt_small_center,
            fmt_tip,
            mem_dc: HDC::default(),
            mem_bmp: HBITMAP::default(),
            buf_w: 0,
            buf_h: 0,
        });
        true
    }

    fn draw_text(
        rt: &ID2D1DCRenderTarget,
        brush: &ID2D1SolidColorBrush,
        fmt: &IDWriteTextFormat,
        text: &str,
        rc: D2D_RECT_F,
        color: D2D1_COLOR_F,
    ) {
        if rc.right <= rc.left || rc.bottom <= rc.top {
            return;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            brush.SetColor(&color);
            rt.DrawText(
                &wide,
                fmt,
                &rc,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}
impl PreviewWindow {
    fn paint_to_dc(&mut self, target_dc: HDC) {
        if !self.ensure_d2d() {
            return;
        }
        self.recompute_layout();

        let playing = self.is_playing();
        let pos = self.position();
        let dur = self.duration();
        let ratio = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };

        let meta_str = self.build_meta_string();
        let theme = self.theme;
        let layout = self.layout;
        let settings = self.settings;
        let hover = self.hover_target;
        let hover_ratio = self.hover_wave_ratio;
        let wf = self.waveform.clone();
        let file_name = self.file_name.clone();
        let err_msg = self.error_msg.clone();
        let kind = self.kind;
        let scale = (self.dpi / 96.0).max(0.5);

        let vid_px_w = ((layout.video_rect.right - layout.video_rect.left) * scale).round().max(0.0) as u32;
        let vid_px_h = ((layout.video_rect.bottom - layout.video_rect.top) * scale).round().max(0.0) as u32;

        let rt_clone = self.res.as_ref().unwrap().rt.clone();
        let video_bmp = if kind == MediaKind::Video {
            if let Some(vp) = &mut self.video_player {
                vp.update_and_get_bitmap(&rt_clone, vid_px_w, vid_px_h).cloned()
            } else {
                None
            }
        } else {
            None
        };

        let mut crc = RECT::default();
        unsafe { let _ = GetClientRect(self.hwnd, &mut crc); }
        let px_w = (crc.right - crc.left).max(1);
        let px_h = (crc.bottom - crc.top).max(1);

        {
            let res_mut = self.res.as_mut().unwrap();
            unsafe {
                res_mut.rt.SetDpi(self.dpi, self.dpi);
                if res_mut.mem_dc.0.is_null() {
                    res_mut.mem_dc = CreateCompatibleDC(Some(target_dc));
                }
                if res_mut.buf_w != px_w || res_mut.buf_h != px_h || res_mut.mem_bmp.0.is_null() {
                    let new_bmp = CreateCompatibleBitmap(target_dc, px_w, px_h);
                    if !new_bmp.0.is_null() {
                        let old = SelectObject(res_mut.mem_dc, new_bmp.into());
                        if !res_mut.mem_bmp.0.is_null() {
                            let _ = DeleteObject(res_mut.mem_bmp.into());
                        } else if !old.0.is_null() {
                            let _ = old;
                        }
                        res_mut.mem_bmp = new_bmp;
                        res_mut.buf_w = px_w;
                        res_mut.buf_h = px_h;
                    }
                }
            }
        }

        let res = self.res.as_ref().unwrap();
        let rt = &res.rt;
        let brush = &res.brush;
        let mem_dc = res.mem_dc;
        let bind_rc = RECT { left: 0, top: 0, right: px_w, bottom: px_h };

        unsafe {
            if let Err(e) = rt.BindDC(mem_dc, &bind_rc) {
                log_msg(&format!("BindDC failed: {e}"));
                return;
            }
            rt.BeginDraw();
            rt.Clear(Some(&theme.bg));

            // 1. Compact Header Strip
            let dot_cy = (layout.header_rect.top + layout.header_rect.bottom) * 0.5;
            brush.SetColor(if playing { &theme.status_playing } else { &theme.status_idle });
            rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: D2D_POINT_2F { X: layout.header_rect.left + 3.5, Y: dot_cy },
                    radiusX: 3.0,
                    radiusY: 3.0,
                },
                brush,
            );
            Self::draw_text(rt, brush, &res.fmt_title, &file_name, layout.name_rect, theme.text_primary);
            Self::draw_text(rt, brush, &res.fmt_meta, &meta_str, layout.meta_rect, theme.text_secondary);

            brush.SetColor(&theme.hairline);
            rt.DrawLine(
                D2D_POINT_2F { X: layout.header_rect.left, Y: layout.divider_y },
                D2D_POINT_2F { X: layout.header_rect.right, Y: layout.divider_y },
                brush,
                1.0,
                None,
            );

            // 2. Video Canvas
            if kind == MediaKind::Video {
                let vr = layout.video_rect;
                let rr = D2D1_ROUNDED_RECT { rect: vr, radiusX: 5.0, radiusY: 5.0 };
                brush.SetColor(&theme.video_bg);
                rt.FillRoundedRectangle(&rr, brush);

                if let Some(bmp) = &video_bmp {
                    let bsz = bmp.GetSize();
                    let vw = vr.right - vr.left;
                    let vh = vr.bottom - vr.top;
                    if bsz.width > 0.0 && bsz.height > 0.0 {
                        let s = (vw / bsz.width).min(vh / bsz.height);
                        let dw = bsz.width * s;
                        let dh = bsz.height * s;
                        let dl = vr.left + (vw - dw) * 0.5;
                        let dt = vr.top + (vh - dh) * 0.5;
                        let dst = rect_f(dl, dt, dl + dw, dt + dh);
                        rt.DrawBitmap(
                            bmp,
                            Some(&dst),
                            1.0,
                            windows::Win32::Graphics::Direct2D::D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                    }
                }

                if !playing && hover == HitTarget::VideoCanvas && err_msg.is_none() {
                    let cx = (vr.left + vr.right) * 0.5;
                    let cy = (vr.top + vr.bottom) * 0.5;
                    let bg_alpha = 0.58f32;
                    brush.SetColor(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: bg_alpha });
                    rt.FillEllipse(
                        &D2D1_ELLIPSE { point: D2D_POINT_2F { X: cx, Y: cy }, radiusX: 18.0, radiusY: 18.0 },
                        brush,
                    );
                    self.draw_play_glyph(rt, brush, cx + 1.0, cy, 6.5, D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.95 });
                }

                brush.SetColor(&theme.border);
                rt.DrawRoundedRectangle(&rr, brush, 1.0, None);
            }

            // 3. Waveform / Scrub Surface
            let wr = layout.wave_rect;
            let wrr = D2D1_ROUNDED_RECT { rect: wr, radiusX: 5.0, radiusY: 5.0 };
            brush.SetColor(&theme.surface);
            rt.FillRoundedRectangle(&wrr, brush);

            let inner_l = wr.left + 8.0;
            let inner_r = (wr.right - 8.0).max(inner_l + 1.0);
            let inner_w = inner_r - inner_l;
            let mid_y = (wr.top + wr.bottom) * 0.5;
            let max_amp = ((wr.bottom - wr.top) * 0.5 - 6.0).max(4.0);

            brush.SetColor(&theme.hairline);
            rt.DrawLine(
                D2D_POINT_2F { X: inner_l, Y: mid_y },
                D2D_POINT_2F { X: inner_r, Y: mid_y },
                brush,
                1.0,
                None,
            );

            if let Some(ref em) = err_msg {
                Self::draw_text(rt, brush, &res.fmt_small_center, em, wr, theme.text_secondary);
            } else if let Some(ref wf_data) = wf {
                let count = wf_data.peaks.len();
                let bar_step = (inner_w / 140.0).clamp(2.2, 3.6);
                let num_bars = ((inner_w / bar_step).floor() as usize).clamp(24, count);
                let actual_step = inner_w / num_bars as f32;
                let bar_w = (actual_step - 1.0).clamp(1.2, 2.5);
                let play_x = inner_l + ratio * inner_w;

                for i in 0..num_bars {
                    let b0 = (i * count) / num_bars;
                    let b1 = (((i + 1) * count) / num_bars).max(b0 + 1).min(count);
                    let mut pk = 0.0f32;
                    let mut rm = 0.0f32;
                    for b in b0..b1 {
                        if wf_data.peaks[b] > pk { pk = wf_data.peaks[b]; }
                        if wf_data.rms[b] > rm { rm = wf_data.rms[b]; }
                    }
                    let x = inner_l + i as f32 * actual_step;
                    let is_played = x + bar_w * 0.5 <= play_x;

                    let h_pk = (pk * max_amp).max(1.2);
                    let h_rm = (rm * max_amp).max(0.8);

                    brush.SetColor(if is_played { &theme.wave_played_peak } else { &theme.wave_unplayed_peak });
                    rt.FillRectangle(&rect_f(x, mid_y - h_pk, x + bar_w, mid_y + h_pk), brush);

                    if h_rm > 1.2 {
                        brush.SetColor(if is_played { &theme.wave_played_rms } else { &theme.wave_unplayed_rms });
                        rt.FillRectangle(&rect_f(x, mid_y - h_rm, x + bar_w, mid_y + h_rm), brush);
                    }
                }
            } else {
                let track_h = 3.0f32;
                let track_rr = D2D1_ROUNDED_RECT {
                    rect: rect_f(inner_l, mid_y - track_h * 0.5, inner_r, mid_y + track_h * 0.5),
                    radiusX: 1.5,
                    radiusY: 1.5,
                };
                brush.SetColor(&theme.wave_unplayed_peak);
                rt.FillRoundedRectangle(&track_rr, brush);
                let px = inner_l + ratio * inner_w;
                if px > inner_l + 1.0 {
                    let fill_rr = D2D1_ROUNDED_RECT {
                        rect: rect_f(inner_l, mid_y - track_h * 0.5, px, mid_y + track_h * 0.5),
                        radiusX: 1.5,
                        radiusY: 1.5,
                    };
                    brush.SetColor(&theme.accent);
                    rt.FillRoundedRectangle(&fill_rr, brush);
                }
            }

            if dur > 0.0 && err_msg.is_none() {
                let px = inner_l + ratio * inner_w;
                brush.SetColor(&theme.accent);
                rt.DrawLine(
                    D2D_POINT_2F { X: px, Y: wr.top + 3.0 },
                    D2D_POINT_2F { X: px, Y: wr.bottom - 3.0 },
                    brush,
                    1.5,
                    None,
                );
            }

            if let Some(hr) = hover_ratio {
                if dur > 0.0 {
                    let hx = inner_l + hr * inner_w;
                    brush.SetColor(&theme.text_secondary);
                    rt.DrawLine(
                        D2D_POINT_2F { X: hx, Y: wr.top + 2.0 },
                        D2D_POINT_2F { X: hx, Y: wr.bottom - 2.0 },
                        brush,
                        1.0,
                        None,
                    );
                    let tip_str = format_time(hr as f64 * dur, dur);
                    let tip_w = 46.0f32;
                    let tip_h = 16.0f32;
                    let tx = (hx - tip_w * 0.5).clamp(wr.left + 4.0, wr.right - tip_w - 4.0);
                    let ty = wr.top + 4.0;
                    let tip_rc = rect_f(tx, ty, tx + tip_w, ty + tip_h);
                    brush.SetColor(&theme.tip_bg);
                    rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: tip_rc, radiusX: 3.0, radiusY: 3.0 }, brush);
                    Self::draw_text(rt, brush, &res.fmt_tip, &tip_str, tip_rc, theme.tip_text);
                }
            }

            brush.SetColor(&theme.border);
            rt.DrawRoundedRectangle(&wrr, brush, 1.0, None);

            // 4. Unified Slim Control Strip
            let play_bg = if playing {
                Some(theme.accent_soft)
            } else if hover == HitTarget::PlayBtn {
                Some(theme.hover_bg)
            } else {
                None
            };
            if let Some(bgc) = play_bg {
                brush.SetColor(&bgc);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: layout.play_btn, radiusX: 4.0, radiusY: 4.0 }, brush);
            }
            let pcx = (layout.play_btn.left + layout.play_btn.right) * 0.5;
            let pcy = (layout.play_btn.top + layout.play_btn.bottom) * 0.5;
            let icon_col = if playing { theme.accent } else { theme.text_primary };
            if playing {
                brush.SetColor(&icon_col);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect_f(pcx - 4.0, pcy - 5.0, pcx - 1.3, pcy + 5.0), radiusX: 1.0, radiusY: 1.0 }, brush);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect_f(pcx + 1.3, pcy - 5.0, pcx + 4.0, pcy + 5.0), radiusX: 1.0, radiusY: 1.0 }, brush);
            } else {
                self.draw_play_glyph(rt, brush, pcx + 0.8, pcy, 5.2, icon_col);
            }

            let time_text = if dur > 0.0 {
                format!("{} / {}", format_time(pos, dur), format_time(dur, dur))
            } else {
                "00:00 / --:--".to_string()
            };
            Self::draw_text(rt, brush, &res.fmt_mono, &time_text, layout.time_rect, theme.text_secondary);

            if settings.loop_enabled {
                brush.SetColor(&theme.accent_soft);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: layout.loop_btn, radiusX: 4.0, radiusY: 4.0 }, brush);
            } else if hover == HitTarget::LoopBtn {
                brush.SetColor(&theme.hover_bg);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: layout.loop_btn, radiusX: 4.0, radiusY: 4.0 }, brush);
            }
            let lcx = (layout.loop_btn.left + layout.loop_btn.right) * 0.5;
            let lcy = (layout.loop_btn.top + layout.loop_btn.bottom) * 0.5;
            self.draw_loop_glyph(rt, brush, &res.stroke_round, lcx, lcy, if settings.loop_enabled { theme.accent } else { theme.text_secondary });

            if layout.speed_rect.right > layout.speed_rect.left {
                let srr = D2D1_ROUNDED_RECT { rect: layout.speed_rect, radiusX: 4.0, radiusY: 4.0 };
                if layout.speed_inline {
                    brush.SetColor(&theme.surface);
                    rt.FillRoundedRectangle(&srr, brush);
                    for i in 0..6 {
                        let sr = layout.speed_segs[i];
                        let active = (SPEEDS[i] - settings.speed).abs() < 0.01;
                        if active {
                            brush.SetColor(&theme.accent_soft);
                            rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect_f(sr.left + 1.0, sr.top + 1.0, sr.right - 1.0, sr.bottom - 1.0), radiusX: 3.0, radiusY: 3.0 }, brush);
                        } else if hover == HitTarget::SpeedSeg(i) {
                            brush.SetColor(&theme.hover_bg);
                            rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect_f(sr.left + 1.0, sr.top + 1.0, sr.right - 1.0, sr.bottom - 1.0), radiusX: 3.0, radiusY: 3.0 }, brush);
                        }
                        Self::draw_text(rt, brush, &res.fmt_small_center, format_speed(SPEEDS[i]), sr, if active { theme.accent } else { theme.text_secondary });
                    }
                    brush.SetColor(&theme.border);
                    rt.DrawRoundedRectangle(&srr, brush, 1.0, None);
                } else {
                    let active = (settings.speed - 1.0).abs() > 0.01;
                    brush.SetColor(if active { &theme.accent_soft } else if hover == HitTarget::SpeedCompact { &theme.hover_bg } else { &theme.surface });
                    rt.FillRoundedRectangle(&srr, brush);
                    brush.SetColor(&theme.border);
                    rt.DrawRoundedRectangle(&srr, brush, 1.0, None);
                    Self::draw_text(rt, brush, &res.fmt_small_center, format_speed(settings.speed), layout.speed_rect, if active { theme.accent } else { theme.text_primary });
                }
            }

            if hover == HitTarget::MuteBtn {
                brush.SetColor(&theme.hover_bg);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: layout.mute_btn, radiusX: 4.0, radiusY: 4.0 }, brush);
            }
            let mcx = (layout.mute_btn.left + layout.mute_btn.right) * 0.5;
            let mcy = (layout.mute_btn.top + layout.mute_btn.bottom) * 0.5;
            let is_muted = settings.muted || settings.volume <= 0.005;
            self.draw_speaker_glyph(rt, brush, &res.stroke_round, mcx, mcy, is_muted, if is_muted { theme.text_secondary } else { theme.text_primary });

            if layout.vol_rect.right > layout.vol_rect.left {
                let vr = layout.vol_rect;
                let vy = (vr.top + vr.bottom) * 0.5;
                let vl = vr.left + 2.0;
                let vrr = vr.right - 2.0;
                let eff_vol = if is_muted { 0.0 } else { settings.volume.clamp(0.0, 1.0) };
                let vx = vl + eff_vol * (vrr - vl);

                brush.SetColor(&theme.wave_unplayed_peak);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect_f(vl, vy - 1.5, vrr, vy + 1.5), radiusX: 1.5, radiusY: 1.5 }, brush);
                if vx > vl + 0.5 {
                    brush.SetColor(&theme.accent);
                    rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect_f(vl, vy - 1.5, vx, vy + 1.5), radiusX: 1.5, radiusY: 1.5 }, brush);
                }
                let knob_r = if hover == HitTarget::VolumeTrack || self.dragging_vol { 4.2 } else { 3.2 };
                brush.SetColor(&theme.accent);
                rt.FillEllipse(&D2D1_ELLIPSE { point: D2D_POINT_2F { X: vx, Y: vy }, radiusX: knob_r, radiusY: knob_r }, brush);
            }

            let arr = D2D1_ROUNDED_RECT { rect: layout.auto_btn, radiusX: 4.0, radiusY: 4.0 };
            if settings.auto_play {
                brush.SetColor(&theme.accent_soft);
                rt.FillRoundedRectangle(&arr, brush);
                brush.SetColor(&theme.accent);
                rt.DrawRoundedRectangle(&arr, brush, 0.8, None);
            } else {
                if hover == HitTarget::AutoPlayBtn {
                    brush.SetColor(&theme.hover_bg);
                    rt.FillRoundedRectangle(&arr, brush);
                }
                brush.SetColor(&theme.border);
                rt.DrawRoundedRectangle(&arr, brush, 1.0, None);
            }
            Self::draw_text(rt, brush, &res.fmt_small_center, "自动", layout.auto_btn, if settings.auto_play { theme.accent } else { theme.text_secondary });

            match rt.EndDraw(None, None) {
                Ok(()) => {
                    let _ = BitBlt(target_dc, 0, 0, px_w, px_h, Some(mem_dc), 0, 0, SRCCOPY);
                }
                Err(e) => {
                    log_msg(&format!("EndDraw failed: {e}"));
                    if let Some(vp) = &mut self.video_player {
                        vp.drop_d2d_resources();
                    }
                    self.res = None;
                }
            }
        }
    }
}
impl PreviewWindow {
    fn build_meta_string(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(4);
        if !self.ext_upper.is_empty() {
            parts.push(self.ext_upper.clone());
        }
        if self.kind == MediaKind::Video {
            if let Some(vp) = &self.video_player {
                let (w, h) = vp.native_size();
                if w > 0 && h > 0 {
                    parts.push(format!("{w}×{h}"));
                }
            }
        } else if let Some(m) = &self.audio_meta {
            if m.sample_rate > 0 {
                let khz = m.sample_rate as f32 / 1000.0;
                if (khz - khz.round()).abs() < 0.05 {
                    parts.push(format!("{}kHz", khz.round() as u32));
                } else {
                    parts.push(format!("{khz:.1}kHz"));
                }
            }
            if m.channels > 0 {
                parts.push(format!("{}ch", m.channels));
            }
        }
        if self.file_size > 0 {
            parts.push(format_size(self.file_size));
        }
        parts.join(" · ")
    }

    fn draw_play_glyph(
        &self,
        rt: &ID2D1DCRenderTarget,
        brush: &ID2D1SolidColorBrush,
        cx: f32,
        cy: f32,
        r: f32,
        color: D2D1_COLOR_F,
    ) {
        unsafe {
            let Ok(geo) = self.d2d_factory.CreatePathGeometry() else { return };
            let Ok(sink) = geo.Open() else { return };
            sink.BeginFigure(D2D_POINT_2F { X: cx - r * 0.75, Y: cy - r }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(D2D_POINT_2F { X: cx + r * 0.95, Y: cy });
            sink.AddLine(D2D_POINT_2F { X: cx - r * 0.75, Y: cy + r });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                brush.SetColor(&color);
                rt.FillGeometry(&geo, brush, None);
            }
        }
    }

    fn draw_loop_glyph(
        &self,
        rt: &ID2D1DCRenderTarget,
        brush: &ID2D1SolidColorBrush,
        stroke: &ID2D1StrokeStyle,
        cx: f32,
        cy: f32,
        color: D2D1_COLOR_F,
    ) {
        unsafe {
            let Ok(geo) = self.d2d_factory.CreatePathGeometry() else { return };
            let Ok(sink) = geo.Open() else { return };
            let r = 5.0f32;
            let start = D2D_POINT_2F { X: cx + r * 0.7, Y: cy + r * 0.7 };
            let end = D2D_POINT_2F { X: cx + r * 0.85, Y: cy - r * 0.45 };
            sink.BeginFigure(start, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: end,
                size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_LARGE,
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.BeginFigure(D2D_POINT_2F { X: end.X - 3.0, Y: end.Y - 0.8 }, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLine(end);
            sink.AddLine(D2D_POINT_2F { X: end.X + 0.6, Y: end.Y - 3.2 });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            if sink.Close().is_ok() {
                brush.SetColor(&color);
                rt.DrawGeometry(&geo, brush, 1.35, stroke);
            }
        }
    }

    fn draw_speaker_glyph(
        &self,
        rt: &ID2D1DCRenderTarget,
        brush: &ID2D1SolidColorBrush,
        stroke: &ID2D1StrokeStyle,
        cx: f32,
        cy: f32,
        muted: bool,
        color: D2D1_COLOR_F,
    ) {
        unsafe {
            let Ok(geo) = self.d2d_factory.CreatePathGeometry() else { return };
            let Ok(sink) = geo.Open() else { return };
            sink.BeginFigure(D2D_POINT_2F { X: cx - 6.0, Y: cy - 2.2 }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(D2D_POINT_2F { X: cx - 3.5, Y: cy - 2.2 });
            sink.AddLine(D2D_POINT_2F { X: cx - 0.5, Y: cy - 4.8 });
            sink.AddLine(D2D_POINT_2F { X: cx - 0.5, Y: cy + 4.8 });
            sink.AddLine(D2D_POINT_2F { X: cx - 3.5, Y: cy + 2.2 });
            sink.AddLine(D2D_POINT_2F { X: cx - 6.0, Y: cy + 2.2 });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            if sink.Close().is_ok() {
                brush.SetColor(&color);
                rt.FillGeometry(&geo, brush, None);
            }

            brush.SetColor(&color);
            if muted {
                rt.DrawLine(D2D_POINT_2F { X: cx + 2.0, Y: cy - 2.5 }, D2D_POINT_2F { X: cx + 6.0, Y: cy + 2.5 }, brush, 1.3, stroke);
                rt.DrawLine(D2D_POINT_2F { X: cx + 6.0, Y: cy - 2.5 }, D2D_POINT_2F { X: cx + 2.0, Y: cy + 2.5 }, brush, 1.3, stroke);
            } else if let Ok(wgeo) = self.d2d_factory.CreatePathGeometry() {
                if let Ok(wsink) = wgeo.Open() {
                    wsink.BeginFigure(D2D_POINT_2F { X: cx + 1.8, Y: cy - 2.6 }, D2D1_FIGURE_BEGIN_HOLLOW);
                    wsink.AddArc(&D2D1_ARC_SEGMENT {
                        point: D2D_POINT_2F { X: cx + 1.8, Y: cy + 2.6 },
                        size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: 3.0, height: 3.0 },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: D2D1_ARC_SIZE_SMALL,
                    });
                    wsink.EndFigure(D2D1_FIGURE_END_OPEN);

                    wsink.BeginFigure(D2D_POINT_2F { X: cx + 4.0, Y: cy - 4.5 }, D2D1_FIGURE_BEGIN_HOLLOW);
                    wsink.AddArc(&D2D1_ARC_SEGMENT {
                        point: D2D_POINT_2F { X: cx + 4.0, Y: cy + 4.5 },
                        size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: 5.2, height: 5.2 },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: D2D1_ARC_SIZE_SMALL,
                    });
                    wsink.EndFigure(D2D1_FIGURE_END_OPEN);
                    if wsink.Close().is_ok() {
                        rt.DrawGeometry(&wgeo, brush, 1.25, stroke);
                    }
                }
            }
        }
    }

    fn hit_test(&self, x: f32, y: f32) -> HitTarget {
        let l = &self.layout;
        if contains(l.wave_rect, x, y) {
            return HitTarget::WaveOrScrub;
        }
        if self.kind == MediaKind::Video && contains(l.video_rect, x, y) {
            return HitTarget::VideoCanvas;
        }
        if contains(l.play_btn, x, y) {
            return HitTarget::PlayBtn;
        }
        if contains(l.loop_btn, x, y) {
            return HitTarget::LoopBtn;
        }
        if l.speed_rect.right > l.speed_rect.left && contains(l.speed_rect, x, y) {
            if l.speed_inline {
                for i in 0..6 {
                    if contains(l.speed_segs[i], x, y) {
                        return HitTarget::SpeedSeg(i);
                    }
                }
            }
            return HitTarget::SpeedCompact;
        }
        if contains(l.mute_btn, x, y) {
            return HitTarget::MuteBtn;
        }
        if l.vol_rect.right > l.vol_rect.left && contains(l.vol_rect, x, y) {
            return HitTarget::VolumeTrack;
        }
        if contains(l.auto_btn, x, y) {
            return HitTarget::AutoPlayBtn;
        }
        HitTarget::None
    }

    fn wave_ratio_from_x(&self, x: f32) -> f32 {
        let il = self.layout.wave_rect.left + 8.0;
        let ir = (self.layout.wave_rect.right - 8.0).max(il + 1.0);
        ((x - il) / (ir - il)).clamp(0.0, 1.0)
    }

    fn vol_from_x(&self, x: f32) -> f32 {
        let vl = self.layout.vol_rect.left + 2.0;
        let vr = (self.layout.vol_rect.right - 2.0).max(vl + 1.0);
        ((x - vl) / (vr - vl)).clamp(0.0, 1.0)
    }

    fn to_dips(&self, lparam: LPARAM) -> (f32, f32) {
        let px = (lparam.0 & 0xFFFF) as i16 as f32;
        let py = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
        let scale = (self.dpi / 96.0).max(0.5);
        (px / scale, py / scale)
    }

    fn window_class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        }
    }

    fn should_capture_space(&self) -> bool {
        if self.hwnd.0.is_null() {
            return false;
        }
        let fg = unsafe { GetForegroundWindow() };
        if fg.0.is_null() {
            return false;
        }
        let root = unsafe { GetAncestor(self.parent_hwnd, GA_ROOT) };
        if root.0.is_null() || fg != root {
            return false;
        }

        let fg_tid = unsafe { GetWindowThreadProcessId(fg, None) };
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(fg_tid, &mut gti) }.is_err() {
            return false;
        }
        let focus = gti.hwndFocus;
        if focus.0.is_null() {
            return false;
        }
        // When our own window has focus, WM_KEYDOWN handles Space directly
        if focus == self.hwnd {
            return false;
        }

        // Strict allowlist: only capture Space when focus is in Explorer's file list (SHELLDLL_DefView)
        let cls = Self::window_class_name(focus);
        if cls != "DirectUIHWND" && cls != "SysListView32" {
            return false;
        }
        let mut cur = focus;
        for _ in 0..6 {
            let Ok(p) = (unsafe { GetParent(cur) }) else { break };
            if p.0.is_null() {
                break;
            }
            if Self::window_class_name(p) == "SHELLDLL_DefView" {
                return true;
            }
            cur = p;
        }
        false
    }

    fn sync_hotkey(&mut self) {
        if self.hwnd.0.is_null() {
            return;
        }
        let want = self.should_capture_space();
        if want && !self.hotkey_registered {
            if unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY_SPACE_ID, HOT_KEY_MODIFIERS(0), VK_SPACE.0 as u32) }.is_ok() {
                self.hotkey_registered = true;
            }
        } else if !want && self.hotkey_registered {
            self.unregister_hotkey();
        }
    }

    fn unregister_hotkey(&mut self) {
        if self.hotkey_registered && !self.hwnd.0.is_null() {
            unsafe { let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_SPACE_ID); }
            self.hotkey_registered = false;
        }
    }
}
impl PreviewWindow {
    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_ERASEBKGND => return Some(LRESULT(1)),

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
                if !hdc.0.is_null() {
                    self.paint_to_dc(hdc);
                }
                unsafe { let _ = EndPaint(self.hwnd, &ps); }
                return Some(LRESULT(0));
            }

            WM_PRINTCLIENT => {
                let hdc = HDC(wparam.0 as *mut _);
                if !hdc.0.is_null() {
                    self.paint_to_dc(hdc);
                }
                return Some(LRESULT(0));
            }

            WM_SIZE => {
                let dpi = unsafe { GetDpiForWindow(self.hwnd) };
                if dpi > 0 {
                    self.dpi = dpi as f32;
                }
                self.recompute_layout();
                self.invalidate();
                return Some(LRESULT(0));
            }

            WM_SETTINGCHANGE => {
                self.theme = detect_theme();
                self.invalidate();
                return Some(LRESULT(0));
            }

            WM_TIMER => {
                if wparam.0 == TIMER_RENDER {
                    if self.is_playing() {
                        self.invalidate();
                    } else if self.kind == MediaKind::Video && self.video_warmup_ticks > 0 {
                        self.video_warmup_ticks -= 1;
                        self.invalidate();
                        if self.video_warmup_ticks == 0 && !self.is_playing() {
                            self.set_render_timer(false);
                        }
                    } else {
                        self.set_render_timer(false);
                        self.invalidate();
                    }
                } else if wparam.0 == TIMER_HOTKEY {
                    self.sync_hotkey();
                }
                return Some(LRESULT(0));
            }

            WM_HOTKEY => {
                if wparam.0 as i32 == HOTKEY_SPACE_ID {
                    self.toggle_play();
                }
                return Some(LRESULT(0));
            }

            WM_APP_AUDIO_META => {
                if let Some(slot) = &self.waveform_slot {
                    if let Ok(m) = slot.meta.lock() {
                        if let Some(meta) = m.clone() {
                            if let Some(ap) = &self.audio_player {
                                ap.set_duration(meta.duration_sec);
                            }
                            self.audio_meta = Some(meta);
                        }
                    }
                }
                self.invalidate();
                return Some(LRESULT(0));
            }

            WM_APP_WAVEFORM_READY => {
                if let Some(slot) = &self.waveform_slot {
                    if let Ok(w) = slot.waveform.lock() {
                        if let Some(wf) = w.clone() {
                            if let Some(ap) = &self.audio_player {
                                ap.set_duration(wf.meta.duration_sec);
                            }
                            self.audio_meta = Some(wf.meta.clone());
                            self.waveform = Some(wf);
                        }
                    }
                    if self.kind == MediaKind::Audio && self.waveform.is_none() {
                        if let Ok(err) = slot.error.lock() {
                            if err.is_some() {
                                self.error_msg = Some("该音频格式暂不支持预览，请双击外部播放".to_string());
                            }
                        }
                    }
                }
                self.invalidate();
                return Some(LRESULT(0));
            }

            WM_APP_PLAY_STATE => {
                let playing = self.is_playing();
                self.set_render_timer(playing);
                self.invalidate();
                return Some(LRESULT(0));
            }

            WM_APP_VIDEO_EVENT => {
                let ev = wparam.0 as u32;
                if let Some(vp) = &mut self.video_player {
                    vp.on_event(ev);
                    if vp.has_error() {
                        self.error_msg = Some("该视频编码暂不被系统 Media Foundation 支持，请双击用外部播放器打开".to_string());
                    }
                    let playing = vp.is_playing();
                    if playing {
                        self.set_render_timer(true);
                    } else {
                        self.video_warmup_ticks = 8;
                        self.set_render_timer(true);
                    }
                }
                self.recompute_layout();
                self.invalidate();
                return Some(LRESULT(0));
            }

            WM_SETCURSOR => {
                let cursor_id = if self.hover_target != HitTarget::None { IDC_HAND } else { IDC_ARROW };
                if let Ok(c) = unsafe { LoadCursorW(None, cursor_id) } {
                    unsafe { SetCursor(Some(c)); }
                }
                return Some(LRESULT(1));
            }

            WM_MOUSEMOVE => {
                if !self.mouse_tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe { let _ = TrackMouseEvent(&mut tme); }
                    self.mouse_tracking = true;
                }
                let (x, y) = self.to_dips(lparam);
                if self.dragging_wave {
                    let r = self.wave_ratio_from_x(x);
                    self.hover_wave_ratio = Some(r);
                    self.seek_to_ratio(r);
                    return Some(LRESULT(0));
                }
                if self.dragging_vol {
                    let v = self.vol_from_x(x);
                    self.settings.volume = v;
                    self.settings.muted = v <= 0.005;
                    self.apply_settings();
                    return Some(LRESULT(0));
                }

                let new_hover = self.hit_test(x, y);
                let new_wave_r = if new_hover == HitTarget::WaveOrScrub {
                    Some(self.wave_ratio_from_x(x))
                } else {
                    None
                };
                if new_hover != self.hover_target || new_wave_r != self.hover_wave_ratio {
                    self.hover_target = new_hover;
                    self.hover_wave_ratio = new_wave_r;
                    self.invalidate();
                }
                return Some(LRESULT(0));
            }

            WM_MOUSELEAVE => {
                self.mouse_tracking = false;
                if self.hover_target != HitTarget::None || self.hover_wave_ratio.is_some() {
                    self.hover_target = HitTarget::None;
                    self.hover_wave_ratio = None;
                    self.invalidate();
                }
                return Some(LRESULT(0));
            }

            WM_LBUTTONDOWN => {
                unsafe { let _ = SetFocus(Some(self.hwnd)); }
                let (x, y) = self.to_dips(lparam);
                let target = self.hit_test(x, y);
                match target {
                    HitTarget::WaveOrScrub => {
                        self.dragging_wave = true;
                        unsafe { SetCapture(self.hwnd); }
                        let r = self.wave_ratio_from_x(x);
                        self.hover_wave_ratio = Some(r);
                        self.seek_to_ratio(r);
                    }
                    HitTarget::VideoCanvas | HitTarget::PlayBtn => {
                        self.toggle_play();
                    }
                    HitTarget::LoopBtn => {
                        self.settings.loop_enabled = !self.settings.loop_enabled;
                        self.apply_settings();
                    }
                    HitTarget::SpeedCompact => {
                        self.settings.speed = self.settings.next_speed();
                        self.apply_settings();
                    }
                    HitTarget::SpeedSeg(idx) => {
                        if let Some(&s) = SPEEDS.get(idx) {
                            self.settings.speed = s;
                            self.apply_settings();
                        }
                    }
                    HitTarget::MuteBtn => {
                        self.settings.muted = !self.settings.muted;
                        if !self.settings.muted && self.settings.volume <= 0.01 {
                            self.settings.volume = 0.7;
                        }
                        self.apply_settings();
                    }
                    HitTarget::VolumeTrack => {
                        self.dragging_vol = true;
                        unsafe { SetCapture(self.hwnd); }
                        let v = self.vol_from_x(x);
                        self.settings.volume = v;
                        self.settings.muted = v <= 0.005;
                        self.apply_settings();
                    }
                    HitTarget::AutoPlayBtn => {
                        self.settings.auto_play = !self.settings.auto_play;
                        self.apply_settings();
                    }
                    HitTarget::None => {}
                }
                return Some(LRESULT(0));
            }

            WM_RBUTTONDOWN => {
                let (x, y) = self.to_dips(lparam);
                if matches!(self.hit_test(x, y), HitTarget::SpeedCompact | HitTarget::SpeedSeg(_)) {
                    self.settings.speed = 1.0;
                    self.apply_settings();
                }
                return Some(LRESULT(0));
            }

            WM_LBUTTONUP => {
                if self.dragging_wave || self.dragging_vol {
                    self.dragging_wave = false;
                    self.dragging_vol = false;
                    unsafe { let _ = ReleaseCapture(); }
                    self.invalidate();
                }
                return Some(LRESULT(0));
            }

            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) as i16) as i32;
                let step = if delta > 0 { 1 } else { -1 };
                match self.hover_target {
                    HitTarget::SpeedCompact | HitTarget::SpeedSeg(_) => {
                        self.settings.speed = self.settings.step_speed(step);
                        self.apply_settings();
                    }
                    HitTarget::VolumeTrack | HitTarget::MuteBtn => {
                        let next = (self.settings.volume + step as f32 * 0.05).clamp(0.0, 1.0);
                        self.settings.volume = next;
                        self.settings.muted = next <= 0.005;
                        self.apply_settings();
                    }
                    HitTarget::WaveOrScrub => {
                        let dur = self.duration();
                        let s = if dur > 0.0 && dur < 15.0 { 1.0 } else { 5.0 };
                        self.seek_relative(step as f64 * s);
                    }
                    _ => {}
                }
                return Some(LRESULT(0));
            }

            WM_KEYDOWN => {
                let vk = wparam.0 as u16;
                if vk == VK_SPACE.0 {
                    self.toggle_play();
                    return Some(LRESULT(0));
                } else if vk == VK_LEFT.0 {
                    let dur = self.duration();
                    let s = if dur > 0.0 && dur < 15.0 { 1.0 } else { 5.0 };
                    self.seek_relative(-s);
                    return Some(LRESULT(0));
                } else if vk == VK_RIGHT.0 {
                    let dur = self.duration();
                    let s = if dur > 0.0 && dur < 15.0 { 1.0 } else { 5.0 };
                    self.seek_relative(s);
                    return Some(LRESULT(0));
                } else if vk == b'L' as u16 {
                    self.settings.loop_enabled = !self.settings.loop_enabled;
                    self.apply_settings();
                    return Some(LRESULT(0));
                } else if vk == b'M' as u16 {
                    self.settings.muted = !self.settings.muted;
                    self.apply_settings();
                    return Some(LRESULT(0));
                } else if vk == VK_OEM_4.0 {
                    // '[' speed down
                    self.settings.speed = self.settings.step_speed(-1);
                    self.apply_settings();
                    return Some(LRESULT(0));
                } else if vk == VK_OEM_6.0 {
                    // ']' speed up
                    self.settings.speed = self.settings.step_speed(1);
                    self.apply_settings();
                    return Some(LRESULT(0));
                } else if vk == VK_UP.0 {
                    self.settings.volume = (self.settings.volume + 0.05).clamp(0.0, 1.0);
                    self.settings.muted = false;
                    self.apply_settings();
                    return Some(LRESULT(0));
                } else if vk == VK_DOWN.0 {
                    self.settings.volume = (self.settings.volume - 0.05).clamp(0.0, 1.0);
                    self.settings.muted = self.settings.volume <= 0.005;
                    self.apply_settings();
                    return Some(LRESULT(0));
                }
            }

            WM_DESTROY => {
                self.unregister_hotkey();
                self.set_render_timer(false);
                return Some(LRESULT(0));
            }

            _ => {}
        }
        None
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA_IDX) } as *mut PreviewWindow;
    if !ptr.is_null() {
        let win = unsafe { &mut *ptr };
        if let Some(res) = win.handle_message(msg, wparam, lparam) {
            return res;
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}