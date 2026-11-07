//! An ambient micro-looper with reverb.
//!
//! Signal chain:
//! 1. Wet: an 8-line feedback delay network reverb (Time, Wet), added to the dry input.
//! 2. Looper: records dry + reverb. Tap Record to start the first take, tap again to close the
//!    loop; later taps overdub at the play head. Speed varispeeds the loop (negative reverses).
//!    Clock runs the looper at a lower internal rate: longer loops, more grit, same pitch.
//! 3. Output: dry + reverb + loop.

use nih_plug::params::persist::PersistentField;
use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

mod editor;

/// Bypass crossfade time in seconds, long enough to avoid clicks
const BYPASS_FADE: f32 = 0.01;

// Reverb. Delay lengths are spread and mutually prime-ish so the echoes don't line up.
const FDN_MS: [f32; 8] = [29.7, 37.1, 41.1, 43.7, 53.3, 59.9, 67.1, 73.1];
const T60_MIN: f32 = 0.3;
const T60_MAX: f32 = 20.0;
/// High cut inside the feedback loop, so the tail darkens as it decays
const DAMP_FC: f32 = 6000.0;

// Looper
/// Buffer length in seconds at full clock; Clock stretches it up to `1 / CLOCK_MIN` times
const LOOP_SECONDS: f32 = 20.0;
/// Lowest clock, as a fraction of the host sample rate
const CLOCK_MIN: f32 = 0.125;
/// The existing loop is turned down by this much on each overdub pass
const OVERDUB_DECAY: f32 = 0.9;
/// Fade at the loop seam, in buffer cells, so the wrap doesn't click
const SEAM_FADE: usize = 256;

/// Topology-preserving one-pole low-pass.
#[derive(Default, Clone, Copy)]
struct OnePole {
    s: f32,
}

impl OnePole {
    /// `g` is `tan(pi * fc / fs)`.
    fn lp(&mut self, x: f32, g: f32) -> f32 {
        let v = (x - self.s) * g / (1.0 + g);
        let y = v + self.s;
        self.s = y + v;
        y
    }
}

fn prewarp(fc: f32, fs: f32) -> f32 {
    (PI * fc.min(fs * 0.45) / fs).tan()
}

/// Feedback delay network: 8 lines mixed through a Hadamard matrix. Left feeds the even lines and
/// right the odd ones, and they're read back the same way, so a stereo source stays wide.
#[derive(Default)]
struct Reverb {
    lines: [Vec<f32>; 8],
    pos: [usize; 8],
    damp: [OnePole; 8],
    g_damp: f32,
}

impl Reverb {
    /// Allocates, so call it from `initialize()` only.
    fn new(fs: f32) -> Self {
        Self {
            lines: FDN_MS.map(|ms| vec![0.0; (ms * 1e-3 * fs) as usize]),
            g_damp: prewarp(DAMP_FC, fs),
            ..Self::default()
        }
    }

    fn clear(&mut self) {
        self.lines.iter_mut().for_each(|l| l.fill(0.0));
        self.damp = Default::default();
    }

    /// Per-line feedback gains for a decay time in seconds.
    fn gains(&self, t60: f32, fs: f32) -> [f32; 8] {
        std::array::from_fn(|i| 10f32.powf(-3.0 * self.lines[i].len() as f32 / fs / t60))
    }

    fn process(&mut self, x: [f32; 2], gains: &[f32; 8]) -> [f32; 2] {
        let mut v: [f32; 8] = std::array::from_fn(|i| self.lines[i][self.pos[i]]);
        let out = [
            (v[0] + v[2] + v[4] + v[6]) * 0.5,
            (v[1] + v[3] + v[5] + v[7]) * 0.5,
        ];

        // Fast Walsh-Hadamard transform, scaled to stay lossless
        let mut h = 1;
        while h < 8 {
            for i in (0..8).step_by(2 * h) {
                for j in i..i + h {
                    (v[j], v[j + h]) = (v[j] + v[j + h], v[j] - v[j + h]);
                }
            }
            h *= 2;
        }

        for i in 0..8 {
            let fb = self.damp[i].lp(v[i] * 8f32.sqrt().recip(), self.g_damp) * gains[i];
            self.lines[i][self.pos[i]] = fb + x[i % 2];
            self.pos[i] = (self.pos[i] + 1) % self.lines[i].len();
        }
        out
    }
}

/// Stereo loop buffer with one head that both plays and records, like tape.
#[derive(Default)]
struct Looper {
    buf: [Vec<f32>; 2],
    /// Loop length in cells, 0 while empty
    len: usize,
    /// Head position in cells
    pos: f32,
    /// Recording the first take, which sets the length
    taking: bool,
    was_recording: bool,
}

impl Looper {
    #[cfg(test)]
    fn new(fs: f32) -> Self {
        let mut lp = Self::default();
        lp.fit(fs);
        lp
    }

    /// Sizes the buffer for a sample rate, keeping whatever loop fits. Allocates.
    fn fit(&mut self, fs: f32) {
        let cells = (LOOP_SECONDS * fs) as usize;
        for b in &mut self.buf {
            b.resize(cells, 0.0);
        }
        self.len = self.len.min(cells);
        if self.pos >= self.len as f32 {
            self.pos = 0.0;
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.pos = 0.0;
        self.taking = false;
    }

    fn has_loop(&self) -> bool {
        self.len > 0
    }

    /// Ends the first take: fade both ends so the seam doesn't click, then play from the top.
    fn close(&mut self) {
        self.taking = false;
        self.len = (self.pos as usize).min(self.buf[0].len());
        let fade = SEAM_FADE.min(self.len / 2);
        for b in &mut self.buf {
            for i in 0..fade {
                let g = i as f32 / fade as f32;
                b[i] *= g;
                b[self.len - 1 - i] *= g;
            }
        }
        self.pos = 0.0;
    }

    /// `rate` is the clock as a fraction of the host rate, `speed` is the playback speed.
    fn process(&mut self, x: [f32; 2], record: bool, speed: f32, rate: f32) -> [f32; 2] {
        if record && !self.was_recording && !self.has_loop() {
            self.taking = true;
            self.pos = 0.0;
        }
        if !record && self.taking {
            self.close();
        }
        self.was_recording = record;

        if self.taking {
            // Sample-and-hold into each cell crossed. No anti-alias filter: the grit at low clock is the point.
            let cap = self.buf[0].len();
            let start = self.pos as usize;
            self.pos += rate;
            for i in start..(self.pos as usize).min(cap) {
                self.buf[0][i] = x[0];
                self.buf[1][i] = x[1];
            }
            if self.pos as usize >= cap {
                self.close();
            }
            return [0.0; 2];
        }
        if !self.has_loop() {
            return [0.0; 2];
        }

        let len = self.len;
        let i = self.pos as usize;
        let frac = self.pos - i as f32;
        let next = (i + 1) % len;
        let out = [0, 1].map(|c| self.buf[c][i] + (self.buf[c][next] - self.buf[c][i]) * frac);

        let old = self.pos.floor() as i64;
        let moved = self.pos + speed * rate;
        if record {
            // Overdub every cell the head enters on this step, in either direction
            let new = moved.floor() as i64;
            let cells = if new > old {
                old + 1..new + 1
            } else {
                new..old
            };
            for k in cells {
                let k = k.rem_euclid(len as i64) as usize;
                for (b, x) in self.buf.iter_mut().zip(x) {
                    b[k] = b[k] * OVERDUB_DECAY + x;
                }
            }
        }
        self.pos = moved.rem_euclid(len as f32);
        // rem_euclid can round up to len itself
        if self.pos >= len as f32 {
            self.pos = 0.0;
        }
        out
    }
}

/// The looper, shared by the audio thread and the plugin state so the loop is saved with the project.
#[derive(Default)]
struct LoopStore(Mutex<Looper>);

impl LoopStore {
    /// Only contended while the host saves or loads state, for one copy of the loop.
    fn lock(&self) -> MutexGuard<'_, Looper> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Saved as the left and right loop cells, base64 little-endian f32.
impl<'a> PersistentField<'a, [String; 2]> for LoopStore {
    fn set(&self, saved: [String; 2]) {
        // A loop that doesn't decode is dropped rather than half-loaded
        let (Some(mut l), Some(mut r)) = (decode(&saved[0]), decode(&saved[1])) else {
            return;
        };
        let len = l.len().min(r.len());
        l.truncate(len);
        r.truncate(len);
        // Restored at whatever rate it was recorded; initialize() sizes the buffer back up
        let mut lp = self.lock();
        lp.buf = [l, r];
        lp.len = len;
        lp.pos = 0.0;
        lp.taking = false;
    }

    fn map<F, R>(&self, f: F) -> R
    where
        F: Fn(&[String; 2]) -> R,
    {
        // Copy under the lock, encode after, so the audio thread waits as little as possible
        let cells = {
            let lp = self.lock();
            lp.buf.clone().map(|mut b| {
                b.truncate(lp.len);
                b
            })
        };
        f(&cells.map(|b| encode(&b)))
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode(x: &[f32]) -> String {
    let bytes: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            s.push(if k <= c.len() {
                B64[(n >> (18 - 6 * k) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

/// `None` unless it's whole finite samples, since it comes from a project file.
fn decode(s: &str) -> Option<Vec<f32>> {
    let mut bytes = Vec::with_capacity(s.len() / 4 * 3);
    let (mut n, mut bits) = (0u32, 0);
    for ch in s.trim_end_matches('=').bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        n = (n << 6 | v as u32) & 0xffff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((n >> bits) as u8);
        }
    }
    if bytes.len() % 4 != 0 {
        return None;
    }
    let x: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect();
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// Maps the Clock knob (0..1) to the looper rate as a fraction of the host rate, an octave per third.
fn clock_rate(clock: f32) -> f32 {
    CLOCK_MIN.powf(1.0 - clock)
}

fn time_to_t60(time: f32) -> f32 {
    T60_MIN * (T60_MAX / T60_MIN).powf(time)
}

/// State shared with the editor.
#[derive(Default)]
pub struct Shared {
    /// Set by the editor's Clear button, taken by `process()`
    clear: AtomicBool,
    /// Whether a loop is recorded, for the LED
    has_loop: AtomicBool,
}

struct Vibe {
    params: Arc<VibeParams>,
    shared: Arc<Shared>,
    sample_rate: f32,
    reverb: Reverb,
    /// 0 = bypassed, 1 = fully processed
    wet: f32,
}

#[derive(Params)]
struct VibeParams {
    #[persist = "editor-size"]
    editor_state: Arc<EguiState>,
    #[persist = "loop"]
    looper: LoopStore,

    #[id = "time"]
    time: FloatParam,
    #[id = "wet"]
    wet: FloatParam,
    #[id = "clock"]
    clock: FloatParam,
    #[id = "loop"]
    loop_level: FloatParam,
    #[id = "speed"]
    speed: FloatParam,
    #[id = "record"]
    record: BoolParam,
    #[id = "bypass"]
    bypass: BoolParam,
}

fn knob(name: &str, default: f32) -> FloatParam {
    FloatParam::new(name, default, FloatRange::Linear { min: 0.0, max: 1.0 })
        .with_smoother(SmoothingStyle::Linear(20.0))
        .with_unit("%")
        .with_value_to_string(formatters::v2s_f32_percentage(0))
        .with_string_to_value(formatters::s2v_f32_percentage())
}

impl Default for VibeParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(editor::WIDTH, editor::HEIGHT),
            looper: LoopStore::default(),
            time: knob("Time", 0.5)
                .with_unit(" s")
                .with_value_to_string(Arc::new(|v| format!("{:.1}", time_to_t60(v))))
                .with_string_to_value(Arc::new(|s| {
                    let t: f32 = s.trim_end_matches(" s").trim().parse().ok()?;
                    Some((t / T60_MIN).ln() / (T60_MAX / T60_MIN).ln())
                })),
            wet: knob("Wet", 0.4),
            clock: knob("Clock", 1.0),
            loop_level: knob("Loop", 0.8),
            // Slow smoothing so speed changes glide like tape
            speed: FloatParam::new(
                "Speed",
                1.0,
                FloatRange::Linear {
                    min: -2.0,
                    max: 2.0,
                },
            )
            .with_smoother(SmoothingStyle::Linear(150.0))
            .with_unit("x")
            .with_value_to_string(formatters::v2s_f32_rounded(2)),
            record: BoolParam::new("Record", false),
            bypass: BoolParam::new("Bypass", false).make_bypass(),
        }
    }
}

impl Default for Vibe {
    fn default() -> Self {
        Self {
            params: Arc::new(VibeParams::default()),
            shared: Arc::new(Shared::default()),
            sample_rate: 48000.0,
            reverb: Reverb::default(),
            wet: 1.0,
        }
    }
}

impl Plugin for Vibe {
    const NAME: &'static str = "Vibe";
    const VENDOR: &'static str = "accraze";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
    ];

    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.shared.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        // Buffers are allocated here because reset() can run on the audio thread
        self.sample_rate = buffer_config.sample_rate;
        self.reverb = Reverb::new(self.sample_rate);
        // Keeps a loop restored from the project
        self.params.looper.lock().fit(self.sample_rate);
        self.reset();
        true
    }

    /// Leaves the loop alone: it belongs to the project, not to playback.
    fn reset(&mut self) {
        self.reverb.clear();
        self.wet = if self.params.bypass.value() { 0.0 } else { 1.0 };
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let mut looper = self.params.looper.lock();
        if self.shared.clear.swap(false, Ordering::Relaxed) {
            looper.clear();
        }
        let fade_step = 1.0 / (BYPASS_FADE * self.sample_rate);

        for mut frame in buffer.iter_samples() {
            let t60 = time_to_t60(self.params.time.smoothed.next());
            let gains = self.reverb.gains(t60, self.sample_rate);
            let wet_level = self.params.wet.smoothed.next();
            let rate = clock_rate(self.params.clock.smoothed.next());
            let loop_level = self.params.loop_level.smoothed.next();
            let speed = self.params.speed.smoothed.next();
            let target = if self.params.bypass.value() { 0.0 } else { 1.0 };
            self.wet += (target - self.wet).clamp(-fade_step, fade_step);

            // Mono runs through the stereo engine with both sides the same
            let stereo = frame.len() > 1;
            let l = *frame.get_mut(0).unwrap();
            let r = if stereo {
                *frame.get_mut(1).unwrap()
            } else {
                l
            };

            // Keep processing while bypassed so the tail and loop carry on underneath
            let verb = self.reverb.process([l, r], &gains);
            let pre = [l + verb[0] * wet_level, r + verb[1] * wet_level];
            let lp = looper.process(pre, self.params.record.value(), speed, rate);
            let out = [0, 1].map(|c| pre[c] + lp[c] * loop_level);

            for (c, sample) in frame.iter_mut().enumerate() {
                let dry = *sample;
                *sample = dry + (out[c] - dry) * self.wet;
            }
        }
        self.shared
            .has_loop
            .store(looper.has_loop(), Ordering::Relaxed);

        ProcessStatus::Normal
    }
}

impl ClapPlugin for Vibe {
    const CLAP_ID: &'static str = "com.accraze.vibe";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Ambient micro-looper with reverb");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Mono,
        ClapFeature::Reverb,
        ClapFeature::Glitch,
    ];
}

impl Vst3Plugin for Vibe {
    const VST3_CLASS_ID: [u8; 16] = *b"VibeMicroLooper!";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Reverb];
}

nih_export_clap!(Vibe);
nih_export_vst3!(Vibe);

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48000.0;

    fn sine(freq: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (2.0 * PI * freq * i as f32 / FS).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|y| y * y).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Positive-going zero crossings per second
    fn pitch(x: &[f32]) -> f32 {
        let ups = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * FS / x.len() as f32
    }

    #[test]
    fn reverb_decays_and_stays_bounded() {
        let mut rv = Reverb::new(FS);
        for time in [0.0, 0.5, 1.0] {
            rv.clear();
            let t60 = time_to_t60(time);
            let gains = rv.gains(t60, FS);
            // One second of loud noise-ish input, then listen to the tail
            let mut peak = 0.0f32;
            for x in sine(331.0, FS as usize) {
                let y = rv.process([x, -x], &gains);
                peak = peak.max(y[0].abs()).max(y[1].abs());
            }
            assert!(peak.is_finite() && peak < 20.0, "time {time}: peak {peak}");
            let tail = |rv: &mut Reverb, n: usize| -> Vec<f32> {
                (0..n).map(|_| rv.process([0.0; 2], &gains)[0]).collect()
            };
            let start = rms(&tail(&mut rv, 4800));
            // Skip to just past T60 and compare
            tail(&mut rv, (t60 * FS) as usize);
            let end = rms(&tail(&mut rv, 4800));
            println!("time {time}: t60 {t60:.1} s, peak {peak:.2}, tail {start:.4} -> {end:.6}");
            assert!(start > 0.01 && end < start * 0.003, "{start} -> {end}");
        }
    }

    /// Records `take` as the first take, then plays the loop back for `n` samples of silence.
    fn record_and_play(take: &[f32], speed: f32, rate: f32, n: usize) -> (Looper, Vec<f32>) {
        let mut lp = Looper::new(FS);
        for &x in take {
            assert_eq!(lp.process([x; 2], true, speed, rate), [0.0; 2]);
        }
        let out = (0..n)
            .map(|_| lp.process([0.0; 2], false, speed, rate)[0])
            .collect();
        (lp, out)
    }

    #[test]
    fn loop_plays_back_the_take() {
        let take = sine(440.0, 24000);
        let (lp, out) = record_and_play(&take, 1.0, 1.0, 48000);
        assert_eq!(lp.len, 24000);
        // Twice round the loop, identical away from the seam fades
        for i in SEAM_FADE..24000 - SEAM_FADE {
            assert!((out[i] - take[i]).abs() < 1e-6);
            assert!((out[i + 24000] - take[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn speed_and_reverse_change_pitch() {
        let take = sine(440.0, 24000);
        let (_, normal) = record_and_play(&take, 1.0, 1.0, 48000);
        let (_, fast) = record_and_play(&take, 2.0, 1.0, 48000);
        let (_, slow) = record_and_play(&take, 0.5, 1.0, 48000);
        let (_, back) = record_and_play(&take, -1.0, 1.0, 48000);
        let p = pitch(&normal);
        assert!((p - 440.0).abs() < 5.0, "normal {p}");
        assert!((pitch(&fast) - 880.0).abs() < 10.0);
        assert!((pitch(&slow) - 220.0).abs() < 5.0);
        assert!((pitch(&back) - 440.0).abs() < 5.0);
        assert!(rms(&back) > 0.3);
    }

    #[test]
    fn low_clock_keeps_pitch_and_stretches_the_buffer() {
        let rate = clock_rate(0.0);
        assert!((rate - CLOCK_MIN).abs() < 1e-6);
        let take = sine(440.0, 24000);
        let (lp, out) = record_and_play(&take, 1.0, rate, 48000);
        assert_eq!(lp.len, (24000.0 * rate) as usize);
        assert!((pitch(&out) - 440.0).abs() < 5.0);
        // A full buffer at the lowest clock holds LOOP_SECONDS / CLOCK_MIN of audio
        let mut lp = Looper::new(FS);
        let n = (LOOP_SECONDS / CLOCK_MIN * FS) as usize;
        for _ in 0..n + 10 {
            lp.process([0.1; 2], true, 1.0, rate);
        }
        assert!(!lp.taking && lp.len == lp.buf[0].len());
    }

    #[test]
    fn overdub_layers_and_decays_the_old_take() {
        let take = vec![0.5; 4800];
        // One sample with Record off closes the take
        let (mut lp, _) = record_and_play(&take, 1.0, 1.0, 1);
        // One pass of overdubbing 0.1 on top
        for _ in 0..4800 {
            lp.process([0.1; 2], true, 1.0, 1.0);
        }
        let out: Vec<f32> = (0..4800)
            .map(|_| lp.process([0.0; 2], false, 1.0, 1.0)[0])
            .collect();
        let mid = out[2400];
        assert!((mid - (0.5 * OVERDUB_DECAY + 0.1)).abs() < 1e-5, "{mid}");
        // Reverse overdub covers every cell too
        for _ in 0..4800 {
            lp.process([0.1; 2], true, -1.0, 1.0);
        }
        assert!(lp.buf[0][SEAM_FADE..4800 - SEAM_FADE]
            .iter()
            .all(|&y| y > mid * OVERDUB_DECAY + 0.09));
    }

    #[test]
    fn loop_survives_save_and_load() {
        let take = sine(440.0, 4800);
        let (lp, _) = record_and_play(&take, 1.0, 1.0, 1);
        let saved = LoopStore(Mutex::new(lp));
        let json = saved
            .map(nih_plug::params::persist::serialize_field)
            .unwrap();
        println!("4800-cell loop saves as {} bytes", json.len());

        let loaded = LoopStore::default();
        loaded.set(nih_plug::params::persist::deserialize_field(&json).unwrap());
        // Reopened at a higher rate: the buffer grows, the loop stays
        loaded.lock().fit(96000.0);
        let (a, b) = (saved.lock(), loaded.lock());
        assert_eq!(b.len, 4800);
        assert_eq!(b.buf[0].len(), (LOOP_SECONDS * 96000.0) as usize);
        assert_eq!(a.buf[0][..4800], b.buf[0][..4800]);
        assert_eq!(a.buf[1][..4800], b.buf[1][..4800]);

        // Odd lengths round-trip, and junk or non-finite data is refused
        for n in 0..4 {
            let x: Vec<f32> = (0..n).map(|i| i as f32 - 1.5).collect();
            assert_eq!(decode(&encode(&x)).unwrap(), x);
        }
        assert!(decode("not base64!").is_none());
        assert!(decode("AAA").is_none());
        assert!(decode(&encode(&[f32::NAN])).is_none());
    }

    #[test]
    fn clear_and_empty_are_silent() {
        let (mut lp, _) = record_and_play(&sine(440.0, 4800), 1.0, 1.0, 0);
        lp.clear();
        assert!(!lp.has_loop());
        for _ in 0..1000 {
            assert_eq!(lp.process([0.3; 2], false, 1.0, 1.0), [0.0; 2]);
        }
    }
}
