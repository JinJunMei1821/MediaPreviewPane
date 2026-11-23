use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_PROPERTIES, ID2D1Bitmap, ID2D1DCRenderTarget};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapLockRead, WICRect,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFMediaEngine, IMFMediaEngineClassFactory,
    IMFMediaEngineEx, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MF_MEDIA_ENGINE_CALLBACK,
    MF_MEDIA_ENGINE_EVENT_CANPLAY, MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR,
    MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY, MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA,
    MF_MEDIA_ENGINE_EVENT_PAUSE, MF_MEDIA_ENGINE_EVENT_PLAYING, MF_MEDIA_ENGINE_EVENT_SEEKED,
    MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION, MFARGB, MFCreateAttributes,
    MFCreateMFByteStreamOnStream, MFSTARTUP_LITE, MFStartup,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{BSTR, Interface, implement};

use crate::settings::log_msg;

pub const WM_APP_VIDEO_EVENT: u32 = 0x8000 + 20;

fn ensure_mf_started() -> Result<(), String> {
    static MF_INIT: OnceLock<Result<(), String>> = OnceLock::new();
    MF_INIT
        .get_or_init(|| unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(|e| format!("MFStartup: {e}"))
        })
        .clone()
}

#[implement(IMFMediaEngineNotify)]
struct MediaEngineCallback {
    notify_hwnd: Arc<AtomicU64>,
    has_error: Arc<AtomicBool>,
    first_frame: Arc<AtomicBool>,
}

impl IMFMediaEngineNotify_Impl for MediaEngineCallback_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 {
            self.has_error.store(true, Ordering::Relaxed);
        } else if event == MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_CANPLAY.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_SEEKED.0 as u32
        {
            self.first_frame.store(true, Ordering::Relaxed);
        }

        let important = event == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_CANPLAY.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_FIRSTFRAMEREADY.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_PLAYING.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_PAUSE.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_ENDED.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_SEEKED.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32;

        if important {
            let h = self.notify_hwnd.load(Ordering::Relaxed) as usize;
            if h != 0 {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(h as *mut _)),
                        WM_APP_VIDEO_EVENT,
                        WPARAM(event as usize),
                        LPARAM(0),
                    );
                }
            }
        }
        Ok(())
    }
}

pub struct VideoPlayer {
    engine: IMFMediaEngine,
    wic_factory: IWICImagingFactory,
    wic_bitmap: Option<IWICBitmap>,
    d2d_bitmap: Option<ID2D1Bitmap>,
    tex_w: u32,
    tex_h: u32,
    notify_hwnd: Arc<AtomicU64>,
    has_error: Arc<AtomicBool>,
    first_frame: Arc<AtomicBool>,
    has_frame_drawn: bool,
    native_w: u32,
    native_h: u32,
    auto_play_pending: bool,
}

impl VideoPlayer {
    pub fn new(
        file_path: Option<&str>,
        mem_data: Option<Arc<Vec<u8>>>,
        ext: &str,
        loop_enabled: bool,
        muted: bool,
        volume: f32,
        speed: f32,
        auto_play: bool,
        notify_hwnd: usize,
    ) -> Result<Self, String> {
        ensure_mf_started()?;

        let hwnd_arc = Arc::new(AtomicU64::new(notify_hwnd as u64));
        let err_arc = Arc::new(AtomicBool::new(false));
        let first_arc = Arc::new(AtomicBool::new(false));

        let cb: IMFMediaEngineNotify = MediaEngineCallback {
            notify_hwnd: hwnd_arc.clone(),
            has_error: err_arc.clone(),
            first_frame: first_arc.clone(),
        }
        .into();

        let engine: IMFMediaEngine = unsafe {
            let factory: IMFMediaEngineClassFactory = CoCreateInstance(
                &CLSID_MFMediaEngineClassFactory,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("MFMediaEngineClassFactory: {e}"))?;

            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 3).map_err(|e| format!("MFCreateAttributes: {e}"))?;
            let attrs = attrs.ok_or_else(|| "null IMFAttributes".to_string())?;
            attrs
                .SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &cb)
                .map_err(|e| format!("SetUnknown callback: {e}"))?;
            attrs
                .SetUINT32(
                    &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                    DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
                )
                .map_err(|e| format!("SetUINT32 output format: {e}"))?;

            factory
                .CreateInstance(0, &attrs)
                .map_err(|e| format!("CreateInstance IMFMediaEngine: {e}"))?
        };

        let wic_factory: IWICImagingFactory = unsafe {
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("WICImagingFactory: {e}"))?
        };

        unsafe {
            let _ = engine.SetAutoPlay(auto_play);
            let _ = engine.SetLoop(loop_enabled);
            let _ = engine.SetMuted(muted);
            let _ = engine.SetVolume(volume.clamp(0.0, 1.0) as f64);
            let _ = engine.SetDefaultPlaybackRate(speed.clamp(0.5, 2.0) as f64);
            let _ = engine.SetPlaybackRate(speed.clamp(0.5, 2.0) as f64);
        }

        // Load from in-memory IStream when available so files are never locked on disk
        let load_res: Result<(), String> = if let Some(bytes) = mem_data {
            unsafe {
                let istream = SHCreateMemStream(Some(bytes.as_slice()))
                    .ok_or_else(|| "SHCreateMemStream returned null".to_string())?;
                let mf_stream = MFCreateMFByteStreamOnStream(&istream)
                    .map_err(|e| format!("MFCreateMFByteStreamOnStream: {e}"))?;
                let engine_ex: IMFMediaEngineEx = engine
                    .cast()
                    .map_err(|e| format!("cast IMFMediaEngineEx: {e}"))?;
                let fake_url = format!("memory://preview{}", if ext.starts_with('.') { ext.to_string() } else { format!(".{ext}") });
                engine_ex
                    .SetSourceFromByteStream(&mf_stream, &BSTR::from(fake_url))
                    .map_err(|e| format!("SetSourceFromByteStream: {e}"))
            }
        } else if let Some(path) = file_path {
            unsafe {
                engine
                    .SetSource(&BSTR::from(path))
                    .map_err(|e| format!("SetSource: {e}"))
            }
        } else {
            Err("no video source".to_string())
        };

        if let Err(e) = load_res {
            log_msg(&format!("VideoPlayer load failed: {e}"));
            unsafe { let _ = engine.Shutdown(); }
            return Err(e);
        }

        Ok(Self {
            engine,
            wic_factory,
            wic_bitmap: None,
            d2d_bitmap: None,
            tex_w: 0,
            tex_h: 0,
            notify_hwnd: hwnd_arc,
            has_error: err_arc,
            first_frame: first_arc,
            has_frame_drawn: false,
            native_w: 0,
            native_h: 0,
            auto_play_pending: auto_play,
        })
    }

    pub fn on_event(&mut self, event: u32) {
        if event == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 as u32
            || event == MF_MEDIA_ENGINE_EVENT_CANPLAY.0 as u32
        {
            let mut w = 0u32;
            let mut h = 0u32;
            if unsafe { self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)) }.is_ok() {
                if w > 0 && h > 0 {
                    self.native_w = w;
                    self.native_h = h;
                }
            }
            if self.auto_play_pending && event == MF_MEDIA_ENGINE_EVENT_CANPLAY.0 as u32 {
                self.auto_play_pending = false;
                self.play();
            }
        }
    }

    pub fn native_size(&self) -> (u32, u32) {
        (self.native_w, self.native_h)
    }

    pub fn has_error(&self) -> bool {
        self.has_error.load(Ordering::Relaxed)
    }

    pub fn has_video(&self) -> bool {
        unsafe { self.engine.HasVideo().as_bool() }
    }

    pub fn is_playing(&self) -> bool {
        unsafe { !self.engine.IsPaused().as_bool() && !self.engine.IsEnded().as_bool() }
    }

    pub fn play(&mut self) {
        unsafe {
            if self.engine.IsEnded().as_bool() {
                let _ = self.engine.SetCurrentTime(0.0);
            }
            let _ = self.engine.Play();
        }
    }

    pub fn pause(&mut self) {
        unsafe {
            let _ = self.engine.Pause();
        }
    }

    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn seek(&mut self, sec: f64) {
        let dur = self.duration();
        let clamped = if dur > 0.0 { sec.clamp(0.0, dur) } else { sec.max(0.0) };
        unsafe {
            let _ = self.engine.SetCurrentTime(clamped);
        }
        self.first_frame.store(true, Ordering::Relaxed);
    }

    pub fn position(&self) -> f64 {
        let t = unsafe { self.engine.GetCurrentTime() };
        if t.is_finite() && t >= 0.0 { t } else { 0.0 }
    }

    pub fn duration(&self) -> f64 {
        let d = unsafe { self.engine.GetDuration() };
        if d.is_finite() && d > 0.0 { d } else { 0.0 }
    }

    pub fn set_loop(&self, v: bool) {
        unsafe { let _ = self.engine.SetLoop(v); }
    }

    pub fn set_muted(&self, v: bool) {
        unsafe { let _ = self.engine.SetMuted(v); }
    }

    pub fn set_volume(&self, v: f32) {
        unsafe { let _ = self.engine.SetVolume(v.clamp(0.0, 1.0) as f64); }
    }

    pub fn set_speed(&self, v: f32) {
        let r = v.clamp(0.5, 2.0) as f64;
        unsafe {
            let _ = self.engine.SetDefaultPlaybackRate(r);
            let _ = self.engine.SetPlaybackRate(r);
        }
    }

    pub fn drop_d2d_resources(&mut self) {
        self.d2d_bitmap = None;
    }

    /// Pull latest decoded video frame (if ready) into our D2D1 bitmap for painting.
    pub fn update_and_get_bitmap(
        &mut self,
        rt: &ID2D1DCRenderTarget,
        target_w: u32,
        target_h: u32,
    ) -> Option<&ID2D1Bitmap> {
        if target_w < 8 || target_h < 8 || !self.has_video() {
            return None;
        }

        // Preserve native aspect ratio inside (target_w, target_h)
        let (bw, bh) = if self.native_w > 0 && self.native_h > 0 {
            let scale = (target_w as f32 / self.native_w as f32)
                .min(target_h as f32 / self.native_h as f32);
            (
                ((self.native_w as f32 * scale).round() as u32).clamp(8, target_w),
                ((self.native_h as f32 * scale).round() as u32).clamp(8, target_h),
            )
        } else {
            (target_w, target_h)
        };

        let size_changed = bw != self.tex_w || bh != self.tex_h || self.wic_bitmap.is_none() || self.d2d_bitmap.is_none();
        if size_changed {
            self.wic_bitmap = unsafe {
                self.wic_factory
                    .CreateBitmap(bw, bh, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)
                    .ok()
            };
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            self.d2d_bitmap = unsafe {
                rt.CreateBitmap(D2D_SIZE_U { width: bw, height: bh }, None, 0, &props).ok()
            };
            self.tex_w = bw;
            self.tex_h = bh;
            self.first_frame.store(true, Ordering::Relaxed);
        }

        let tick_ready = unsafe { self.engine.OnVideoStreamTick() }
            .map(|pts| pts >= 0)
            .unwrap_or(false);
        let force = self.first_frame.swap(false, Ordering::Relaxed) || !self.has_frame_drawn;

        if tick_ready || force {
            if let (Some(wic), Some(d2d)) = (&self.wic_bitmap, &self.d2d_bitmap) {
                let dst_rect = RECT {
                    left: 0,
                    top: 0,
                    right: bw as i32,
                    bottom: bh as i32,
                };
                let border = MFARGB {
                    rgbBlue: 16,
                    rgbGreen: 18,
                    rgbRed: 22,
                    rgbAlpha: 255,
                };
                let transferred = unsafe {
                    self.engine
                        .TransferVideoFrame(wic, None, &dst_rect, Some(&border))
                        .is_ok()
                };
                if transferred {
                    let lock_rc = WICRect {
                        X: 0,
                        Y: 0,
                        Width: bw as i32,
                        Height: bh as i32,
                    };
                    if let Ok(lock) = unsafe { wic.Lock(&lock_rc, WICBitmapLockRead.0 as u32) } {
                        let mut buf_size = 0u32;
                        let mut data_ptr: *mut u8 = std::ptr::null_mut();
                        if let Ok(stride) = unsafe { lock.GetStride() } {
                            if unsafe { lock.GetDataPointer(&mut buf_size, &mut data_ptr).is_ok() && !data_ptr.is_null() } {
                            let _ = unsafe { d2d.CopyFromMemory(None, data_ptr.cast(), stride) };
                                self.has_frame_drawn = true;
                            }
                        }
                    }
                }
            }
        }

        if self.has_frame_drawn {
            self.d2d_bitmap.as_ref()
        } else {
            None
        }
    }

    pub fn shutdown(&mut self) {
        self.notify_hwnd.store(0, Ordering::Relaxed);
        self.d2d_bitmap = None;
        self.wic_bitmap = None;
        unsafe {
            let _ = self.engine.Shutdown();
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.shutdown();
    }
}