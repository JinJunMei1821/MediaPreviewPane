use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::core::w;

pub const SPEEDS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

#[derive(Debug, Clone, Copy)]
pub struct UserSettings {
    pub loop_enabled: bool,
    pub speed: f32,
    pub volume: f32,
    pub muted: bool,
    pub auto_play: bool,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            loop_enabled: false,
            speed: 1.0,
            volume: 0.9,
            muted: false,
            auto_play: false,
        }
    }
}

impl UserSettings {
    pub fn clamp_speed(s: f32) -> f32 {
        let mut best = 1.0f32;
        let mut best_diff = f32::MAX;
        for &cand in &SPEEDS {
            let d = (cand - s).abs();
            if d < best_diff {
                best_diff = d;
                best = cand;
            }
        }
        best
    }

    pub fn next_speed(&self) -> f32 {
        let idx = SPEEDS
            .iter()
            .position(|&s| (s - self.speed).abs() < 0.01)
            .unwrap_or(2);
        SPEEDS[(idx + 1) % SPEEDS.len()]
    }

    pub fn step_speed(&self, delta: i32) -> f32 {
        let idx = SPEEDS
            .iter()
            .position(|&s| (s - self.speed).abs() < 0.01)
            .unwrap_or(2) as i32;
        let next = (idx + delta).clamp(0, (SPEEDS.len() - 1) as i32) as usize;
        SPEEDS[next]
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("MediaPreviewPane");
        if fs::create_dir_all(&p).is_ok() {
            return p;
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(profile)
            .join("AppData")
            .join("LocalLow")
            .join("MediaPreviewPane");
        let _ = fs::create_dir_all(&p);
        return p;
    }
    std::env::temp_dir().join("MediaPreviewPane")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.ini")
}

pub fn load_settings() -> UserSettings {
    let mut s = UserSettings::default();
    let Ok(text) = fs::read_to_string(settings_path()) else {
        return s;
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        match k {
            "loop" => s.loop_enabled = v == "1" || v.eq_ignore_ascii_case("true"),
            "speed" => {
                if let Ok(val) = v.parse::<f32>() {
                    s.speed = UserSettings::clamp_speed(val);
                }
            }
            "volume" => {
                if let Ok(val) = v.parse::<f32>() {
                    s.volume = val.clamp(0.0, 1.0);
                }
            }
            "muted" => s.muted = v == "1" || v.eq_ignore_ascii_case("true"),
            "auto_play" => s.auto_play = v == "1" || v.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    s
}

pub fn save_settings(s: &UserSettings) {
    let content = format!(
        "loop={}\nspeed={:.2}\nvolume={:.3}\nmuted={}\nauto_play={}\n",
        if s.loop_enabled { 1 } else { 0 },
        UserSettings::clamp_speed(s.speed),
        s.volume.clamp(0.0, 1.0),
        if s.muted { 1 } else { 0 },
        if s.auto_play { 1 } else { 0 },
    );
    let _ = fs::write(settings_path(), content);
}

static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn log_msg(msg: &str) {
    let Ok(_g) = LOG_LOCK.lock() else { return };
    let path = data_dir().join("preview.log");
    // Keep log tiny (< 64 KB)
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 64 * 1024 {
            let _ = fs::remove_file(&path);
        }
    }
    use std::io::Write;
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() % 86_400_000)
            .unwrap_or(0);
        let h = (ms / 3_600_000) % 24;
        let m = (ms / 60_000) % 60;
        let s = (ms / 1_000) % 60;
        let frac = ms % 1_000;
        let _ = writeln!(f, "[{:02}:{:02}:{:02}.{:03}] {}", h, m, s, frac, msg);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub is_dark: bool,
    pub bg: D2D1_COLOR_F,
    pub surface: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    pub hairline: D2D1_COLOR_F,
    pub text_primary: D2D1_COLOR_F,
    pub text_secondary: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    pub accent_soft: D2D1_COLOR_F,
    pub accent_fill: D2D1_COLOR_F,
    pub wave_unplayed_peak: D2D1_COLOR_F,
    pub wave_unplayed_rms: D2D1_COLOR_F,
    pub wave_played_peak: D2D1_COLOR_F,
    pub wave_played_rms: D2D1_COLOR_F,
    pub hover_bg: D2D1_COLOR_F,
    pub tip_bg: D2D1_COLOR_F,
    pub tip_text: D2D1_COLOR_F,
    pub status_playing: D2D1_COLOR_F,
    pub status_idle: D2D1_COLOR_F,
    pub video_bg: D2D1_COLOR_F,
}

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

fn read_reg_dword(subkey: windows::core::PCWSTR, value: windows::core::PCWSTR) -> Option<u32> {
    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let mut kind = REG_DWORD;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value,
            RRF_RT_REG_DWORD,
            Some(&mut kind),
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if status.is_ok() { Some(data) } else { None }
}

pub fn detect_theme() -> Theme {
    let light = read_reg_dword(
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("AppsUseLightTheme"),
    )
    .unwrap_or(1)
        != 0;

    let accent_abgr = read_reg_dword(
        w!("Software\\Microsoft\\Windows\\DWM"),
        w!("AccentColor"),
    );

    let (ar, ag, ab) = if let Some(c) = accent_abgr {
        let r = (c & 0xFF) as u8;
        let g = ((c >> 8) & 0xFF) as u8;
        let b = ((c >> 16) & 0xFF) as u8;
        if light {
            // Ensure enough contrast on light backgrounds
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            if lum > 170.0 {
                ((r as f32 * 0.65) as u8, (g as f32 * 0.65) as u8, (b as f32 * 0.65) as u8)
            } else {
                (r, g, b)
            }
        } else {
            // Lift dark accents for dark backgrounds
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            if lum < 110.0 {
                (
                    (r as f32 + (255.0 - r as f32) * 0.42) as u8,
                    (g as f32 + (255.0 - g as f32) * 0.42) as u8,
                    (b as f32 + (255.0 - b as f32) * 0.42) as u8,
                )
            } else {
                (r, g, b)
            }
        }
    } else if light {
        (0, 103, 192) // Fluent Blue
    } else {
        (76, 194, 255) // Fluent Sky
    };

    if light {
        Theme {
            is_dark: false,
            bg: rgba(248, 249, 250, 1.0),
            surface: rgba(255, 255, 255, 1.0),
            border: rgba(224, 227, 232, 1.0),
            hairline: rgba(232, 235, 240, 1.0),
            text_primary: rgba(26, 29, 34, 1.0),
            text_secondary: rgba(102, 109, 120, 1.0),
            accent: rgba(ar, ag, ab, 1.0),
            accent_soft: rgba(ar, ag, ab, 0.13),
            accent_fill: rgba(ar, ag, ab, 0.22),
            wave_unplayed_peak: rgba(208, 214, 222, 1.0),
            wave_unplayed_rms: rgba(180, 188, 200, 1.0),
            wave_played_peak: rgba(ar, ag, ab, 0.52),
            wave_played_rms: rgba(ar, ag, ab, 0.95),
            hover_bg: rgba(0, 0, 0, 0.055),
            tip_bg: rgba(28, 32, 38, 0.92),
            tip_text: rgba(255, 255, 255, 1.0),
            status_playing: rgba(22, 163, 74, 1.0),
            status_idle: rgba(156, 163, 175, 1.0),
            video_bg: rgba(18, 20, 24, 1.0),
        }
    } else {
        Theme {
            is_dark: true,
            bg: rgba(25, 25, 25, 1.0),
            surface: rgba(32, 33, 36, 1.0),
            border: rgba(50, 53, 59, 1.0),
            hairline: rgba(44, 46, 52, 1.0),
            text_primary: rgba(236, 238, 242, 1.0),
            text_secondary: rgba(150, 156, 166, 1.0),
            accent: rgba(ar, ag, ab, 1.0),
            accent_soft: rgba(ar, ag, ab, 0.18),
            accent_fill: rgba(ar, ag, ab, 0.28),
            wave_unplayed_peak: rgba(62, 66, 74, 1.0),
            wave_unplayed_rms: rgba(86, 92, 104, 1.0),
            wave_played_peak: rgba(ar, ag, ab, 0.50),
            wave_played_rms: rgba(ar, ag, ab, 0.95),
            hover_bg: rgba(255, 255, 255, 0.08),
            tip_bg: rgba(240, 242, 246, 0.94),
            tip_text: rgba(20, 22, 26, 1.0),
            status_playing: rgba(52, 211, 153, 1.0),
            status_idle: rgba(108, 114, 126, 1.0),
            video_bg: rgba(12, 13, 16, 1.0),
        }
    }
}