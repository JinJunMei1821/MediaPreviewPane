use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::common::Tier;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;
use symphonia_opus::OpusDecoder;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::settings::log_msg;
use crate::wsola::WsolaStretcher;

pub const WAVEFORM_BUCKETS: usize = 360;

pub const WM_APP_AUDIO_META: u32 = 0x8000 + 10;
pub const WM_APP_WAVEFORM_READY: u32 = 0x8000 + 11;
pub const WM_APP_PLAY_STATE: u32 = 0x8000 + 12;

pub struct ArcCursor {
    data: Arc<Vec<u8>>,
    pos: u64,
}

impl ArcCursor {
    pub fn new(data: Arc<Vec<u8>>) -> Self {
        Self { data, pos: 0 }
    }
}

impl Read for ArcCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.data.len() as u64;
        if self.pos >= len {
            return Ok(0);
        }
        let avail = (len - self.pos) as usize;
        let n = avail.min(buf.len());
        let start = self.pos as usize;
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ArcCursor {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = self.data.len() as i64;
        let next = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::End(off) => len + off,
            SeekFrom::Current(off) => self.pos as i64 + off,
        };
        if next < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

impl MediaSource for ArcCursor {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

fn codec_registry() -> &'static CodecRegistry {
    static REG: OnceLock<CodecRegistry> = OnceLock::new();
    REG.get_or_init(|| {
        let mut r = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut r);
        r.register_audio_decoder_at_tier::<OpusDecoder>(Tier::Fallback);
        r
    })
}

#[derive(Clone, Debug)]
pub struct AudioMeta {
    pub duration_sec: f64,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug)]
pub struct WaveformData {
    pub peaks: Vec<f32>,
    pub rms: Vec<f32>,
    pub meta: AudioMeta,
}

#[derive(Hash, PartialEq, Eq, Clone)]
pub struct CacheKey {
    pub id: String,
    pub size: u64,
    pub mtime_ns: u128,
}

struct WaveformCache {
    map: HashMap<CacheKey, Arc<WaveformData>>,
    order: std::collections::VecDeque<CacheKey>,
}

impl WaveformCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: std::collections::VecDeque::new(),
        }
    }
    fn get(&self, key: &CacheKey) -> Option<Arc<WaveformData>> {
        self.map.get(key).cloned()
    }
    fn insert(&mut self, key: CacheKey, val: Arc<WaveformData>) {
        if self.map.contains_key(&key) {
            self.map.insert(key, val);
            return;
        }
        while self.order.len() >= 256 {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, val);
    }
}

fn global_cache() -> &'static Mutex<WaveformCache> {
    static CACHE: OnceLock<Mutex<WaveformCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(WaveformCache::new()))
}

pub fn lookup_cached_waveform(key: &CacheKey) -> Option<Arc<WaveformData>> {
    global_cache().lock().ok()?.get(key)
}

struct OpenedStream {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    duration_sec: f64,
    time_base: Option<symphonia::core::units::TimeBase>,
}

fn open_symphonia(data: Arc<Vec<u8>>, ext: &str) -> Result<OpenedStream, String> {
    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext.trim_start_matches('.'));
    }
    let mss = MediaSourceStream::new(Box::new(ArcCursor::new(data)), Default::default());
    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("probe failed: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .or_else(|| format.first_track(TrackType::Audio))
        .ok_or_else(|| "no audio track".to_string())?;

    let track_id = track.id;
    let time_base = track.time_base;
    let num_frames = track.num_frames;
    let track_dur = track.duration;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "no audio codec params".to_string())?
        .clone();

    let decoder = codec_registry()
        .make_audio_decoder(&codec_params, &AudioDecoderOptions::default())
        .map_err(|e| format!("decoder init failed: {e}"))?;

    let sample_rate = codec_params.sample_rate.unwrap_or(48000);
    let channels = codec_params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(2)
        .max(1);

    let mut duration_sec = 0.0f64;
    if let (Some(tb), Some(d)) = (time_base, track_dur) {
        if let Some(t) = tb.calc_duration(d) {
            duration_sec = t.as_secs_f64();
        }
    } else if let Some(nf) = num_frames {
        if sample_rate > 0 {
            duration_sec = nf as f64 / sample_rate as f64;
        }
    }

    Ok(OpenedStream {
        format,
        decoder,
        track_id,
        sample_rate,
        channels,
        duration_sec,
        time_base,
    })
}

pub struct WaveformResultSlot {
    pub meta: Mutex<Option<AudioMeta>>,
    pub waveform: Mutex<Option<Arc<WaveformData>>>,
    pub error: Mutex<Option<String>>,
}

impl WaveformResultSlot {
    pub fn new() -> Self {
        Self {
            meta: Mutex::new(None),
            waveform: Mutex::new(None),
            error: Mutex::new(None),
        }
    }
}

pub fn spawn_waveform_scan(
    data: Arc<Vec<u8>>,
    ext: String,
    cache_key: Option<CacheKey>,
    slot: Arc<WaveformResultSlot>,
    cancel: Arc<AtomicBool>,
    notify_hwnd: usize,
) {
    if let Some(ref k) = cache_key {
        if let Some(cached) = lookup_cached_waveform(k) {
            if let Ok(mut m) = slot.meta.lock() {
                *m = Some(cached.meta.clone());
            }
            if let Ok(mut w) = slot.waveform.lock() {
                *w = Some(cached);
            }
            if notify_hwnd != 0 {
                unsafe {
                    let _ = PostMessageW(Some(HWND(notify_hwnd as *mut _)), WM_APP_WAVEFORM_READY, WPARAM(0), LPARAM(0));
                }
            }
            return;
        }
    }

    thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let mut stream = match open_symphonia(data, &ext) {
            Ok(s) => s,
            Err(e) => {
                if let Ok(mut err) = slot.error.lock() {
                    *err = Some(e);
                }
                if notify_hwnd != 0 && !cancel.load(Ordering::Relaxed) {
                    unsafe {
                        let _ = PostMessageW(Some(HWND(notify_hwnd as *mut _)), WM_APP_WAVEFORM_READY, WPARAM(0), LPARAM(0));
                    }
                }
                return;
            }
        };

        let init_meta = AudioMeta {
            duration_sec: stream.duration_sec,
            sample_rate: stream.sample_rate,
            channels: stream.channels,
        };
        if let Ok(mut m) = slot.meta.lock() {
            *m = Some(init_meta);
        }
        if notify_hwnd != 0 && !cancel.load(Ordering::Relaxed) {
            unsafe {
                let _ = PostMessageW(Some(HWND(notify_hwnd as *mut _)), WM_APP_AUDIO_META, WPARAM(0), LPARAM(0));
            }
        }

        let mut chunks: Vec<(u32, f32, f64)> = Vec::with_capacity(4096);
        let mut interleaved: Vec<f32> = Vec::new();
        let mut total_frames: u64 = 0;
        let mut actual_sr = stream.sample_rate;
        let mut actual_ch = stream.channels;

        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let pkt = match stream.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(SymError::ResetRequired) => {
                    stream.decoder.reset();
                    continue;
                }
                Err(_) => break,
            };
            if pkt.track_id != stream.track_id {
                continue;
            }
            let decoded = match stream.decoder.decode(&pkt) {
                Ok(d) => d,
                Err(SymError::DecodeError(_)) => continue,
                Err(_) => break,
            };
            let frames = decoded.frames();
            if frames == 0 {
                continue;
            }
            let spec = decoded.spec();
            if spec.rate() > 0 {
                actual_sr = spec.rate();
            }
            let ch = spec.channels().count().max(1);
            actual_ch = ch as u16;

            interleaved.clear();
            decoded.copy_to_vec_interleaved::<f32>(&mut interleaved);

            let sub_size = 512usize;
            let mut offset = 0usize;
            while offset < frames {
                let end = (offset + sub_size).min(frames);
                let mut peak = 0.0f32;
                let mut sum_sq = 0.0f64;
                for f in offset..end {
                    let base = f * ch;
                    let mut frame_abs = 0.0f32;
                    for c in 0..ch {
                        let v = interleaved[base + c].abs();
                        if v > frame_abs {
                            frame_abs = v;
                        }
                    }
                    if frame_abs > peak {
                        peak = frame_abs;
                    }
                    sum_sq += (frame_abs as f64) * (frame_abs as f64);
                }
                chunks.push(((end - offset) as u32, peak, sum_sq));
                offset = end;
            }
            total_frames += frames as u64;
        }

        if cancel.load(Ordering::Relaxed) {
            return;
        }

        let mut peaks = vec![0.0f32; WAVEFORM_BUCKETS];
        let mut rms = vec![0.0f32; WAVEFORM_BUCKETS];
        let mut bucket_sq = vec![0.0f64; WAVEFORM_BUCKETS];
        let mut bucket_cnt = vec![0u64; WAVEFORM_BUCKETS];

        if total_frames > 0 {
            let mut cursor = 0u64;
            for &(n_frames, p, sq) in &chunks {
                let b = ((cursor as u128 * WAVEFORM_BUCKETS as u128) / total_frames as u128)
                    .min((WAVEFORM_BUCKETS - 1) as u128) as usize;
                if p > peaks[b] {
                    peaks[b] = p;
                }
                bucket_sq[b] += sq;
                bucket_cnt[b] += n_frames as u64;
                cursor += n_frames as u64;
            }
        }

        let mut global_max = 0.001f32;
        for i in 0..WAVEFORM_BUCKETS {
            if bucket_cnt[i] > 0 {
                rms[i] = (bucket_sq[i] / bucket_cnt[i] as f64).sqrt() as f32;
            }
            if peaks[i] > global_max {
                global_max = peaks[i];
            }
        }

        let norm = if global_max > 0.02 { global_max } else { 1.0 };
        for i in 0..WAVEFORM_BUCKETS {
            peaks[i] = (peaks[i] / norm).clamp(0.0, 1.0);
            rms[i] = (rms[i] / norm).clamp(0.0, peaks[i]);
        }

        let exact_dur = if total_frames > 0 && actual_sr > 0 {
            total_frames as f64 / actual_sr as f64
        } else {
            stream.duration_sec
        };

        let wf = Arc::new(WaveformData {
            peaks,
            rms,
            meta: AudioMeta {
                duration_sec: exact_dur,
                sample_rate: actual_sr,
                channels: actual_ch,
            },
        });

        if let Some(k) = cache_key {
            if let Ok(mut c) = global_cache().lock() {
                c.insert(k, wf.clone());
            }
        }

        if let Ok(mut m) = slot.meta.lock() {
            *m = Some(wf.meta.clone());
        }
        if let Ok(mut w) = slot.waveform.lock() {
            *w = Some(wf);
        }

        if notify_hwnd != 0 && !cancel.load(Ordering::Relaxed) {
            unsafe {
                let _ = PostMessageW(Some(HWND(notify_hwnd as *mut _)), WM_APP_WAVEFORM_READY, WPARAM(0), LPARAM(0));
            }
        }
    });
}
enum PlayerCmd {
    Play,
    Pause,
    Seek(f64),
    Shutdown,
}

struct SharedAudioState {
    is_playing: AtomicBool,
    loop_enabled: AtomicBool,
    muted: AtomicBool,
    volume_bits: AtomicU32,
    speed_bits: AtomicU32,
    position_bits: AtomicU64,
    duration_bits: AtomicU64,
    notify_hwnd: AtomicU64,
    cmds: Mutex<Vec<PlayerCmd>>,
}

impl SharedAudioState {
    fn new(
        duration_sec: f64,
        loop_enabled: bool,
        muted: bool,
        volume: f32,
        speed: f32,
        hwnd: usize,
    ) -> Self {
        Self {
            is_playing: AtomicBool::new(false),
            loop_enabled: AtomicBool::new(loop_enabled),
            muted: AtomicBool::new(muted),
            volume_bits: AtomicU32::new(volume.to_bits()),
            speed_bits: AtomicU32::new(speed.to_bits()),
            position_bits: AtomicU64::new(0.0f64.to_bits()),
            duration_bits: AtomicU64::new(duration_sec.to_bits()),
            notify_hwnd: AtomicU64::new(hwnd as u64),
            cmds: Mutex::new(Vec::new()),
        }
    }

    fn push_cmd(&self, cmd: PlayerCmd) {
        if let Ok(mut q) = self.cmds.lock() {
            q.push(cmd);
        }
    }

    fn notify_ui(&self) {
        let h = self.notify_hwnd.load(Ordering::Relaxed) as usize;
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_PLAY_STATE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

pub struct AudioPlayer {
    data: Arc<Vec<u8>>,
    ext: String,
    shared: Arc<SharedAudioState>,
    worker: Option<JoinHandle<()>>,
}

impl AudioPlayer {
    pub fn new(
        data: Arc<Vec<u8>>,
        ext: String,
        duration_sec: f64,
        loop_enabled: bool,
        muted: bool,
        volume: f32,
        speed: f32,
        notify_hwnd: usize,
    ) -> Self {
        Self {
            data,
            ext,
            shared: Arc::new(SharedAudioState::new(
                duration_sec,
                loop_enabled,
                muted,
                volume,
                speed,
                notify_hwnd,
            )),
            worker: None,
        }
    }

    fn ensure_worker(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let data = self.data.clone();
        let ext = self.ext.clone();
        let shared = self.shared.clone();
        self.worker = Some(thread::spawn(move || {
            run_wasapi_loop(data, &ext, shared);
        }));
    }

    pub fn play(&mut self) {
        self.shared.is_playing.store(true, Ordering::Relaxed);
        self.ensure_worker();
        self.shared.push_cmd(PlayerCmd::Play);
    }

    pub fn pause(&mut self) {
        self.shared.is_playing.store(false, Ordering::Relaxed);
        if self.worker.is_some() {
            self.shared.push_cmd(PlayerCmd::Pause);
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
        self.shared.position_bits.store(clamped.to_bits(), Ordering::Relaxed);
        if self.worker.is_some() {
            self.shared.push_cmd(PlayerCmd::Seek(clamped));
        }
    }

    pub fn set_duration(&self, dur: f64) {
        if dur > 0.0 {
            self.shared.duration_bits.store(dur.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.is_playing.load(Ordering::Relaxed)
    }

    pub fn position(&self) -> f64 {
        f64::from_bits(self.shared.position_bits.load(Ordering::Relaxed))
    }

    pub fn duration(&self) -> f64 {
        f64::from_bits(self.shared.duration_bits.load(Ordering::Relaxed))
    }

    pub fn set_loop(&self, v: bool) {
        self.shared.loop_enabled.store(v, Ordering::Relaxed);
    }

    pub fn set_muted(&self, v: bool) {
        self.shared.muted.store(v, Ordering::Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.volume_bits.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_speed(&self, v: f32) {
        self.shared.speed_bits.store(v.clamp(0.5, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.shared.notify_hwnd.store(0, Ordering::Relaxed);
        self.shared.is_playing.store(false, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            self.shared.push_cmd(PlayerCmd::Shutdown);
            let _ = handle.join();
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn seek_stream(stream: &mut OpenedStream, data: &Arc<Vec<u8>>, ext: &str, target_sec: f64) -> f64 {
    let target_sec = target_sec.max(0.0);
    let seek_time = Time::try_from_secs_f64(target_sec).unwrap_or_default();
    let res = stream.format.seek(
        SeekMode::Accurate,
        SeekTo::Time {
            time: seek_time,
            track_id: Some(stream.track_id),
        },
    );
    match res {
        Ok(seeked) => {
            stream.decoder.reset();
            if let Some(tb) = stream.time_base {
                tb.calc_time(seeked.actual_ts).map(|t| t.as_secs_f64()).unwrap_or(target_sec)
            } else {
                target_sec
            }
        }
        Err(_) => {
            if let Ok(fresh) = open_symphonia(data.clone(), ext) {
                *stream = fresh;
            }
            0.0
        }
    }
}

fn decode_next_stereo(
    stream: &mut OpenedStream,
    tmp_raw: &mut Vec<f32>,
    out_stereo: &mut Vec<f32>,
) -> Option<usize> {
    out_stereo.clear();
    loop {
        let pkt = match stream.format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => return None,
            Err(SymError::ResetRequired) => {
                stream.decoder.reset();
                continue;
            }
            Err(_) => return None,
        };
        if pkt.track_id != stream.track_id {
            continue;
        }
        let decoded = match stream.decoder.decode(&pkt) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue,
            Err(_) => return None,
        };
        let frames = decoded.frames();
        if frames == 0 {
            continue;
        }
        let ch = decoded.spec().channels().count().max(1);
        tmp_raw.clear();
        decoded.copy_to_vec_interleaved::<f32>(tmp_raw);

        out_stereo.reserve(frames * 2);
        if ch == 2 {
            out_stereo.extend_from_slice(tmp_raw);
        } else if ch == 1 {
            for &s in tmp_raw.iter() {
                out_stereo.push(s);
                out_stereo.push(s);
            }
        } else {
            for f in 0..frames {
                let base = f * ch;
                out_stereo.push(tmp_raw[base]);
                out_stereo.push(tmp_raw[base + 1]);
            }
        }
        return Some(frames);
    }
}

unsafe fn init_wasapi(sample_rate: u32) -> Result<(IAudioClient, IAudioRenderClient, u32, HANDLE), String> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| format!("IMMDeviceEnumerator: {e}"))?;

    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| format!("GetDefaultAudioEndpoint: {e}"))?;

    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| format!("IAudioClient Activate: {e}"))?;

    let wfx = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: 2,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: 0x3,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    };

    let buf_duration_hns: i64 = 400_000;
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                buf_duration_hns,
                0,
                (&wfx.Format) as *const WAVEFORMATEX,
                None,
            )
            .map_err(|e| format!("IAudioClient::Initialize: {e}"))?;
    }

    let event = unsafe { CreateEventW(None, false, false, None) }
        .map_err(|e| format!("CreateEventW: {e}"))?;

    if let Err(e) = unsafe { client.SetEventHandle(event) } {
        unsafe { let _ = CloseHandle(event); }
        return Err(format!("SetEventHandle: {e}"));
    }

    let buf_frames = unsafe { client.GetBufferSize() }
        .map_err(|e| format!("GetBufferSize: {e}"))?;

    let render: IAudioRenderClient = unsafe { client.GetService() }
        .map_err(|e| format!("GetService IAudioRenderClient: {e}"))?;

    Ok((client, render, buf_frames, event))
}

fn run_wasapi_loop(data: Arc<Vec<u8>>, ext: &str, shared: Arc<SharedAudioState>) {
    let com_hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    let mut stream = match open_symphonia(data.clone(), ext) {
        Ok(s) => s,
        Err(e) => {
            log_msg(&format!("AudioPlayer open error: {e}"));
            shared.is_playing.store(false, Ordering::Relaxed);
            shared.notify_ui();
            if com_hr.is_ok() {
                unsafe { CoUninitialize(); }
            }
            return;
        }
    };

    let sr = stream.sample_rate.max(8000);
    let (client, render, buf_frames, event) = match unsafe { init_wasapi(sr) } {
        Ok(v) => v,
        Err(e) => {
            log_msg(&format!("WASAPI init error: {e}"));
            shared.is_playing.store(false, Ordering::Relaxed);
            shared.notify_ui();
            if com_hr.is_ok() {
                unsafe { CoUninitialize(); }
            }
            return;
        }
    };

    let mut stretcher = WsolaStretcher::new(sr);
    let mut tmp_raw: Vec<f32> = Vec::with_capacity(4096);
    let mut tmp_stereo: Vec<f32> = Vec::with_capacity(4096);
    let mut out_chunk: Vec<f32> = vec![0.0; buf_frames as usize * 2];

    let initial_pos = f64::from_bits(shared.position_bits.load(Ordering::Relaxed));
    let mut current_pos_frames: f64 = if initial_pos > 0.01 {
        let actual = seek_stream(&mut stream, &data, ext, initial_pos);
        actual * sr as f64
    } else {
        0.0
    };

    let mut eof = false;
    let mut was_started = false;
    let mut fade_gain: f32 = 0.0;
    let fade_step: f32 = 1.0 / ((sr as f32 * 0.005).max(32.0));

    loop {
        let mut shutdown = false;
        let cmds: Vec<PlayerCmd> = {
            if let Ok(mut q) = shared.cmds.lock() {
                std::mem::take(&mut *q)
            } else {
                Vec::new()
            }
        };
        for cmd in cmds {
            match cmd {
                PlayerCmd::Shutdown => shutdown = true,
                PlayerCmd::Play => {
                    if eof {
                        let actual = seek_stream(&mut stream, &data, ext, 0.0);
                        current_pos_frames = actual * sr as f64;
                        stretcher.reset();
                        eof = false;
                    }
                    fade_gain = 0.0;
                    if !was_started {
                        if unsafe { client.Start() }.is_ok() {
                            was_started = true;
                        }
                    }
                }
                PlayerCmd::Pause => {
                    if was_started {
                        let _ = unsafe { client.Stop() };
                        was_started = false;
                    }
                }
                PlayerCmd::Seek(sec) => {
                    let actual = seek_stream(&mut stream, &data, ext, sec);
                    current_pos_frames = actual * sr as f64;
                    stretcher.reset();
                    eof = false;
                    fade_gain = 0.0;
                    if !was_started {
                        let _ = unsafe { client.Reset() };
                    }
                }
            }
        }
        if shutdown {
            break;
        }

        let playing = shared.is_playing.load(Ordering::Relaxed);
        if !playing {
            if was_started {
                let _ = unsafe { client.Stop() };
                was_started = false;
            }
            thread::sleep(std::time::Duration::from_millis(12));
            continue;
        }

        if !was_started {
            if unsafe { client.Start() }.is_ok() {
                was_started = true;
            } else {
                thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
        }

        unsafe {
            WaitForSingleObject(event, 20);
        }

        let padding = match unsafe { client.GetCurrentPadding() } {
            Ok(p) => p,
            Err(_) => {
                shared.is_playing.store(false, Ordering::Relaxed);
                shared.notify_ui();
                break;
            }
        };

        let avail_frames = buf_frames.saturating_sub(padding) as usize;
        if avail_frames == 0 {
            continue;
        }

        let speed = f32::from_bits(shared.speed_bits.load(Ordering::Relaxed)).clamp(0.5, 2.0);
        let volume = f32::from_bits(shared.volume_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0);
        let muted = shared.muted.load(Ordering::Relaxed);
        let target_gain = if muted { 0.0 } else { volume.powf(1.5) };

        while stretcher.output_frames() < avail_frames {
            if !eof && stretcher.input_frames() < 8192 {
                match decode_next_stereo(&mut stream, &mut tmp_raw, &mut tmp_stereo) {
                    Some(_) => stretcher.push_stereo(&tmp_stereo),
                    None => eof = true,
                }
            }
            let prev_out = stretcher.output_frames();
            let _ = stretcher.process(speed, eof);
            if eof && stretcher.output_frames() == prev_out {
                break;
            }
            if !eof && stretcher.output_frames() == prev_out && stretcher.input_frames() >= 8192 {
                break;
            }
        }

        let needed = avail_frames.min(out_chunk.len() / 2);
        let got = stretcher.pop_stereo(&mut out_chunk[..needed * 2]);
        if got < needed {
            out_chunk[got * 2..needed * 2].fill(0.0);
        }

        if let Ok(ptr) = unsafe { render.GetBuffer(needed as u32) } {
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, needed * 2) };
            for i in 0..needed {
                if fade_gain < 1.0 {
                    fade_gain = (fade_gain + fade_step).min(1.0);
                }
                let g = target_gain * fade_gain;
                dst[i * 2] = (out_chunk[i * 2] * g).clamp(-1.0, 1.0);
                dst[i * 2 + 1] = (out_chunk[i * 2 + 1] * g).clamp(-1.0, 1.0);
            }
            let _ = unsafe { render.ReleaseBuffer(needed as u32, 0) };
        }

        current_pos_frames += (got as f64) * (speed as f64);
        let dur = f64::from_bits(shared.duration_bits.load(Ordering::Relaxed));
        let pos_sec = (current_pos_frames / sr as f64).max(0.0);
        let clamped_sec = if dur > 0.0 { pos_sec.min(dur) } else { pos_sec };
        shared.position_bits.store(clamped_sec.to_bits(), Ordering::Relaxed);

        if eof && stretcher.output_frames() == 0 && got < needed {
            if shared.loop_enabled.load(Ordering::Relaxed) {
                let actual = seek_stream(&mut stream, &data, ext, 0.0);
                current_pos_frames = actual * sr as f64;
                stretcher.reset();
                eof = false;
                fade_gain = 0.0;
                shared.position_bits.store(actual.to_bits(), Ordering::Relaxed);
            } else {
                shared.is_playing.store(false, Ordering::Relaxed);
                shared.position_bits.store(0.0f64.to_bits(), Ordering::Relaxed);
                if was_started {
                    let _ = unsafe { client.Stop() };
                    let _ = unsafe { client.Reset() };
                    was_started = false;
                }
                shared.notify_ui();
            }
        }
    }

    if was_started {
        let _ = unsafe { client.Stop() };
    }
    unsafe {
        let _ = CloseHandle(event);
        if com_hr.is_ok() {
            CoUninitialize();
        }
    }
}