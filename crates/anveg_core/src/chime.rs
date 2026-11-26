//! The sound played when Claude finishes a turn, synthesised here as a mono
//! 16-bit WAV so no audio file or audio library is needed.
//!
//! "Bass riff" (chosen by the owner from several candidates): a funky
//! plucked bass climbing C2, E♭2, F2, G2, landing on a deep bowl at C2 with
//! an 808-style sub underneath, in a small room reverb. 2.8 s, peak about
//! -7 dBFS.
//!
//! The instruments are additive: a plucked bass (six harmonics, the higher
//! ones fading faster), a bowl (a fundamental with inharmonic overtones),
//! and an 808 (a sine gliding down to its pitch, gently saturated so
//! laptop speakers still hear it). The reverb is a classic Schroeder room:
//! four feedback combs in parallel, then two allpasses.

use std::f64::consts::TAU;

pub const SAMPLE_RATE: u32 = 44_100;
const RATE: f64 = SAMPLE_RATE as f64;
/// Length of the chime, in seconds.
pub const DURATION: f64 = 2.8;
/// The loudest sample, as a fraction of full scale (about -7 dBFS).
const PEAK: f64 = 0.45;
/// How much reverb is mixed in.
const WET: f64 = 0.22;
/// The final fade, in seconds.
const FADE: f64 = 0.15;

#[derive(Clone, Copy)]
enum Instrument {
    PluckBass,
    LowBowl,
    Kick808,
}

struct Note {
    /// Semitones from A4 (440 Hz).
    semitones: i32,
    start: f64,
    amp: f64,
    instrument: Instrument,
}

const C2: i32 = -33;
const E_FLAT2: i32 = -30;
const F2: i32 = -28;
const G2: i32 = -26;

const NOTES: &[Note] = &[
    Note { semitones: C2, start: 0.0, amp: 0.8, instrument: Instrument::PluckBass },
    Note { semitones: E_FLAT2, start: 0.14, amp: 0.75, instrument: Instrument::PluckBass },
    Note { semitones: F2, start: 0.28, amp: 0.75, instrument: Instrument::PluckBass },
    Note { semitones: G2, start: 0.42, amp: 0.8, instrument: Instrument::PluckBass },
    Note { semitones: C2, start: 0.6, amp: 0.9, instrument: Instrument::LowBowl },
    Note { semitones: C2, start: 0.6, amp: 0.7, instrument: Instrument::Kick808 },
];

fn freq(semitones: i32) -> f64 {
    440.0 * 2f64.powf(f64::from(semitones) / 12.0)
}

/// A smooth attack and an exponential decay.
fn env(t: f64, attack: f64, decay: f64) -> f64 {
    (1.0 - (-t / attack).exp()) * (-t / decay).exp()
}

/// The bowl's partials: (frequency ratio, amplitude, relative decay).
const BOWL: &[(f64, f64, f64)] = &[
    (1.0, 1.0, 1.0),
    (2.0, 0.55, 0.7),
    (3.01, 0.30, 0.45),
    (4.2, 0.12, 0.3),
    (5.4, 0.05, 0.2),
];

fn low_bowl(f: f64, t: f64) -> f64 {
    BOWL.iter()
        .map(|&(ratio, amp, rel)| amp * env(t, 0.008, 1.2 * rel) * (TAU * f * ratio * t).sin())
        .sum()
}

fn pluck_bass(f: f64, t: f64) -> f64 {
    (1..=6)
        .map(|n| {
            let n = f64::from(n);
            (1.0 / n.powf(1.3)) * (TAU * f * n * t).sin() * env(t, 0.003, 0.5 / n.powf(0.6))
        })
        .sum()
}

/// The 808 needs its phase accumulated sample by sample (its pitch glides).
struct Kick808 {
    f: f64,
    phase_sum: f64,
}

impl Kick808 {
    fn next(&mut self, t: f64) -> f64 {
        let freq = self.f * (1.0 + 1.5 * (-t / 0.03).exp());
        self.phase_sum += freq;
        let tone = (TAU * self.phase_sum / RATE).sin() * env(t, 0.001, 0.7);
        (1.8 * tone).tanh() / 1.8f64.tanh()
    }
}

fn dry() -> Vec<f64> {
    let n = (DURATION * RATE) as usize;
    let mut out = vec![0.0; n];
    for note in NOTES {
        let f = freq(note.semitones);
        let mut kick = Kick808 { f, phase_sum: 0.0 };
        for (i, sample) in out.iter_mut().enumerate() {
            let t = i as f64 / RATE - note.start;
            if t < 0.0 {
                continue;
            }
            let value = match note.instrument {
                Instrument::PluckBass => pluck_bass(f, t),
                Instrument::LowBowl => low_bowl(f, t),
                Instrument::Kick808 => kick.next(t),
            };
            *sample += note.amp * value;
        }
    }
    out
}

/// A small room: four feedback combs in parallel, then two allpasses.
fn reverb(x: &[f64]) -> Vec<f64> {
    let delay = |ms: f64| (RATE * ms / 1000.0) as usize;
    let mut out = vec![0.0; x.len()];
    for (ms, feedback) in [(29.7, 0.77), (37.1, 0.75), (41.1, 0.73), (43.7, 0.71)] {
        let d = delay(ms);
        let mut y = vec![0.0; x.len()];
        for i in 0..x.len() {
            y[i] = x[i] + if i >= d { feedback * y[i - d] } else { 0.0 };
        }
        for (o, v) in out.iter_mut().zip(&y) {
            *o += v;
        }
    }
    for o in &mut out {
        *o /= 4.0;
    }
    for (ms, g) in [(5.0, 0.7), (1.7, 0.7)] {
        let d = delay(ms);
        let mut y = vec![0.0; out.len()];
        for i in 0..out.len() {
            let (dx, dy) = if i >= d { (out[i - d], y[i - d]) } else { (0.0, 0.0) };
            y[i] = -g * out[i] + dx + g * dy;
        }
        out = y;
    }
    out
}

/// The chime's samples, from -1.0 to 1.0.
pub fn samples() -> Vec<f64> {
    let dry = dry();
    let wet = reverb(&dry);
    let mut mix: Vec<f64> = dry.iter().zip(&wet).map(|(d, w)| d + WET * w).collect();
    // A fade from 1 to 0 over the last samples, as numpy's linspace does.
    let fade = (FADE * RATE) as usize;
    let len = mix.len();
    for k in 0..fade.min(len) {
        let gain = if fade > 1 {
            1.0 - k as f64 / (fade - 1) as f64
        } else {
            0.0
        };
        mix[len - fade + k] *= gain;
    }
    let max = mix.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
    if max > 0.0 {
        for s in &mut mix {
            *s *= PEAK / max;
        }
    }
    mix
}

/// The chime as a WAV file (PCM, mono, 16-bit, 44.1 kHz).
pub fn finished_wav() -> Vec<u8> {
    let pcm: Vec<i16> = samples()
        .into_iter()
        .map(|s| (s.clamp(-1.0, 1.0) * f64::from(i16::MAX)).round() as i16)
        .collect();
    wav(&pcm, SAMPLE_RATE)
}

fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wav_is_well_formed() {
        let wav = finished_wav();
        let samples = (DURATION * RATE) as usize;
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + samples * 2);
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, samples * 2);
    }

    #[test]
    fn it_is_soft_starts_without_a_click_and_ends_silent() {
        let s = samples();
        let peak = s.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
        assert!((peak - PEAK).abs() < 1e-9, "peak {peak}");
        assert!(s[0].abs() < 1e-9);
        assert!(s[..20].iter().all(|x| x.abs() < 0.1));
        assert!(s.last().is_some_and(|x| x.abs() < 1e-9));
        // No jumps (clicks) anywhere.
        let jump = s.windows(2).fold(0.0_f64, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(jump < 0.05, "jump {jump}");
        // The bowl and the 808 still ring a second after they land.
        let at = (1.6 * RATE) as usize;
        let ring = s[at..at + 2000].iter().fold(0.0_f64, |m, x| m.max(x.abs()));
        assert!(ring > 0.03, "ring {ring}");
    }

    #[test]
    fn the_notes_are_the_riff() {
        assert!((freq(C2) - 65.406_391_325).abs() < 1e-6);
        assert!((freq(G2) - 97.998_858_995).abs() < 1e-6);
    }
}
