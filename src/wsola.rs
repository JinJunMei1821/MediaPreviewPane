use std::collections::VecDeque;

/// Lightweight pitch-preserving WSOLA time-stretcher for stereo interleaved f32 audio.
/// At speed ~= 1.0 it acts as a zero-overhead passthrough.
pub struct WsolaStretcher {
    sample_rate: u32,
    win_frames: usize,
    half_win: usize,
    search_radius: usize,
    hann: Vec<f32>,
    /// Interleaved stereo input queue (2 floats per frame)
    in_buf: VecDeque<f32>,
    /// Overlap-add tail from previous synthesis frame (half_win * 2 floats)
    prev_tail: Vec<f32>,
    /// Natural continuation reference for cross-correlation (half_win * 2 floats)
    natural_ref: Vec<f32>,
    /// Ready output samples (interleaved stereo)
    out_buf: VecDeque<f32>,
    /// Fractional input-frame read cursor
    in_pos: f64,
    initialized: bool,
}

impl WsolaStretcher {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(8000);
        // ~24ms window, forced even
        let mut win_frames = ((sr as usize) * 24 / 1000).clamp(512, 2048);
        if win_frames % 2 != 0 {
            win_frames += 1;
        }
        let half_win = win_frames / 2;
        let search_radius = ((sr as usize) * 8 / 1000).clamp(96, 480);

        let mut hann = Vec::with_capacity(win_frames);
        for i in 0..win_frames {
            let phase = std::f32::consts::PI * (i as f32) / (win_frames as f32);
            let s = phase.sin();
            hann.push(s * s);
        }

        Self {
            sample_rate: sr,
            win_frames,
            half_win,
            search_radius,
            hann,
            in_buf: VecDeque::with_capacity(win_frames * 8),
            prev_tail: vec![0.0; half_win * 2],
            natural_ref: vec![0.0; half_win * 2],
            out_buf: VecDeque::with_capacity(win_frames * 4),
            in_pos: 0.0,
            initialized: false,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn reset(&mut self) {
        self.in_buf.clear();
        self.out_buf.clear();
        self.prev_tail.fill(0.0);
        self.natural_ref.fill(0.0);
        self.in_pos = 0.0;
        self.initialized = false;
    }

    pub fn push_stereo(&mut self, samples: &[f32]) {
        self.in_buf.extend(samples.iter().copied());
    }

    pub fn input_frames(&self) -> usize {
        self.in_buf.len() / 2
    }

    pub fn output_frames(&self) -> usize {
        self.out_buf.len() / 2
    }

    /// Process available input frames at `speed` (0.5..=2.0).
    /// Returns number of source input frames consumed (for accurate playback clock).
    pub fn process(&mut self, speed: f32, eof: bool) -> u64 {
        let speed = speed.clamp(0.5, 2.0);
        let mut consumed_frames = 0u64;

        // Fast zero-overhead passthrough when speed == 1.0x
        if (speed - 1.0).abs() < 0.005 && !self.initialized {
            let avail_frames = self.in_buf.len() / 2;
            if avail_frames > 0 {
                let n = avail_frames * 2;
                self.out_buf.reserve(n);
                for _ in 0..n {
                    if let Some(v) = self.in_buf.pop_front() {
                        self.out_buf.push_back(v);
                    }
                }
                consumed_frames += avail_frames as u64;
            }
            return consumed_frames;
        }

        // If transitioning back to 1.0x from stretched state, drain cleanly first
        if (speed - 1.0).abs() < 0.005 && self.initialized {
            self.out_buf.extend(self.prev_tail.iter().copied());
            self.prev_tail.fill(0.0);
            self.natural_ref.fill(0.0);
            self.in_pos = 0.0;
            self.initialized = false;
            return self.process(1.0, eof);
        }

        let needed_frames = self.win_frames + self.search_radius * 2 + self.half_win;
        loop {
            let avail_frames = self.in_buf.len() / 2;
            if !eof && avail_frames < needed_frames {
                break;
            }
            if avail_frames < self.win_frames {
                if eof {
                    if self.initialized {
                        self.out_buf.extend(self.prev_tail.iter().copied());
                        self.prev_tail.fill(0.0);
                        self.initialized = false;
                    }
                    let rem = self.in_buf.len() / 2;
                    for _ in 0..(rem * 2) {
                        if let Some(v) = self.in_buf.pop_front() {
                            self.out_buf.push_back(v);
                        }
                    }
                    consumed_frames += rem as u64;
                }
                break;
            }

            // Ensure contiguous slice for fast correlation
            self.in_buf.make_contiguous();
            let (slice, _) = self.in_buf.as_slices();
            let total_frames = slice.len() / 2;

            let target = self.in_pos.round() as isize;
            let best_start = if !self.initialized {
                0usize
            } else {
                let min_s = (target - self.search_radius as isize).max(0) as usize;
                let max_s = ((target + self.search_radius as isize) as usize)
                    .min(total_frames.saturating_sub(self.win_frames));
                if min_s >= max_s {
                    min_s.min(total_frames.saturating_sub(self.win_frames))
                } else {
                    self.find_best_offset(slice, min_s, max_s)
                }
            };

            if best_start + self.win_frames > total_frames {
                break;
            }

            if !self.initialized {
                // Emit first half directly, save second half weighted as prev_tail
                for i in 0..self.half_win {
                    let idx = (best_start + i) * 2;
                    self.out_buf.push_back(slice[idx]);
                    self.out_buf.push_back(slice[idx + 1]);
                }
                for i in 0..self.half_win {
                    let idx = (best_start + self.half_win + i) * 2;
                    let w = self.hann[self.half_win + i];
                    self.prev_tail[i * 2] = slice[idx] * w;
                    self.prev_tail[i * 2 + 1] = slice[idx + 1] * w;
                }
                self.initialized = true;
            } else {
                // Overlap-add first half with prev_tail
                for i in 0..self.half_win {
                    let idx = (best_start + i) * 2;
                    let w = self.hann[i];
                    let l = self.prev_tail[i * 2] + slice[idx] * w;
                    let r = self.prev_tail[i * 2 + 1] + slice[idx + 1] * w;
                    self.out_buf.push_back(l);
                    self.out_buf.push_back(r);
                }
                // Store second half weighted as next prev_tail
                for i in 0..self.half_win {
                    let idx = (best_start + self.half_win + i) * 2;
                    let w = self.hann[self.half_win + i];
                    self.prev_tail[i * 2] = slice[idx] * w;
                    self.prev_tail[i * 2 + 1] = slice[idx + 1] * w;
                }
            }

            // Save natural continuation right after best_start + half_win for next correlation
            let nat_start = best_start + self.half_win;
            if nat_start + self.half_win <= total_frames {
                let s = nat_start * 2;
                self.natural_ref.copy_from_slice(&slice[s..s + self.half_win * 2]);
            }

            // Advance source read pointer by half_win * speed
            let advance = (self.half_win as f64) * (speed as f64);
            self.in_pos += advance;

            // Drop input frames that are safely behind the search window
            let safe_drop = (self.in_pos.floor() as isize - self.search_radius as isize).max(0) as usize;
            let drop_frames = safe_drop.min(total_frames);
            if drop_frames > 0 {
                self.in_buf.drain(..drop_frames * 2);
                self.in_pos -= drop_frames as f64;
                consumed_frames += drop_frames as u64;
            }
        }

        consumed_frames
    }

    /// Cross-correlate mono-summed signal with sub-sampling stride for ultra-low CPU
    fn find_best_offset(&self, slice: &[f32], min_s: usize, max_s: usize) -> usize {
        let len = self.half_win;
        let stride = 4usize; // 4x faster, indistinguishable quality for speech & music
        let mut best_pos = min_s;
        let mut best_score = f32::NEG_INFINITY;

        // Coarse search (step 2) then fine search around best
        let mut pos = min_s;
        while pos <= max_s {
            let mut dot = 0.0f32;
            let mut norm = 1e-6f32;
            let mut i = 0;
            while i < len {
                let a = self.natural_ref[i * 2] + self.natural_ref[i * 2 + 1];
                let idx = (pos + i) * 2;
                let b = slice[idx] + slice[idx + 1];
                dot += a * b;
                norm += b * b;
                i += stride;
            }
            let score = dot / norm.sqrt();
            if score > best_score {
                best_score = score;
                best_pos = pos;
            }
            pos += 2;
        }

        let fine_lo = best_pos.saturating_sub(2).max(min_s);
        let fine_hi = (best_pos + 2).min(max_s);
        for p in fine_lo..=fine_hi {
            let mut dot = 0.0f32;
            let mut norm = 1e-6f32;
            let mut i = 0;
            while i < len {
                let a = self.natural_ref[i * 2] + self.natural_ref[i * 2 + 1];
                let idx = (p + i) * 2;
                let b = slice[idx] + slice[idx + 1];
                dot += a * b;
                norm += b * b;
                i += 2;
            }
            let score = dot / norm.sqrt();
            if score > best_score {
                best_score = score;
                best_pos = p;
            }
        }

        best_pos
    }

    pub fn pop_stereo(&mut self, dst: &mut [f32]) -> usize {
        let count = dst.len().min(self.out_buf.len());
        let count = count - (count % 2);
        for v in dst.iter_mut().take(count) {
            *v = self.out_buf.pop_front().unwrap_or(0.0);
        }
        count / 2
    }
}