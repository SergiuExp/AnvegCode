//! The sound played when Claude finishes a turn: a soft, low two-note
//! chime (G3, then C3 a fifth below, which sounds "done"), synthesised
//! here as a mono 16-bit WAV so no audio file or audio library is needed.
//!
//! Each note is a struck-bowl tone: a fundamental with a slightly detuned
//! twin (a slow shimmer), plus a few quieter overtones that fade faster than
//! the fundamental, a 6 ms attack (no click) and an exponential decay.

use std::f64::consts::TAU;

pub const SAMPLE_RATE: u32 = 44_100;
/// Length of the chime, in seconds.
pub const DURATION: f64 = 2.4;
/// The loudest sample, as a fraction of full scale (about -7 dBFS).
const PEAK: f64 = 0.45;

struct Note {
    freq: f64,
    start: f64,
    amp: f64,
    /// Time constant of the fundamental's decay, in seconds.
    decay: f64,
}

/// (frequency ratio, relative amplitude, relative decay).
const PARTIALS: &[(f64, f64, f64)] = &[
    (1.0, 1.0, 1.0),
    (2.0, 0.30, 0.55),
    (3.01, 0.11, 0.35),
    (4.2, 0.04, 0.22),
];

const NOTES: &[Note] = &[
    // G3
    Note {
        freq: 196.0,
        start: 0.0,
        amp: 0.55,
        decay: 0.55,
    },
    // C3
    Note {
        freq: 130.81,
        start: 0.17,
        amp: 1.0,
        decay: 0.85,
    },
];

fn note_sample(note: &Note, t: f64) -> f64 {
    let t = t - note.start;
    if t < 0.0 {
        return 0.0;
    }
    let attack = 1.0 - (-t / 0.006).exp();
    let mut sum = 0.0;
    for &(ratio, amp, decay) in PARTIALS {
        let f = note.freq * ratio;
        let env = (-t / (note.decay * decay)).exp();
        let mut tone = (TAU * f * t).sin();
        if ratio == 1.0 {
            // A twin 0.7 Hz higher: a slow, warm beating.
            tone = 0.7 * tone + 0.3 * (TAU * (f + 0.7) * t).sin();
        }
        sum += amp * env * tone;
    }
    note.amp * attack * sum
}

/// The chime's samples, from -1.0 to 1.0.
pub fn samples() -> Vec<f64> {
    let n = (DURATION * f64::from(SAMPLE_RATE)) as usize;
    let fade = (0.15 * f64::from(SAMPLE_RATE)) as usize;
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(SAMPLE_RATE);
            NOTES.iter().map(|note| note_sample(note, t)).sum()
        })
        .collect();
    // A short fade so the end is silent whatever is left of the decay.
    for (k, s) in out.iter_mut().rev().take(fade).enumerate() {
        *s *= k as f64 / fade as f64;
    }
    let max = out.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
    if max > 0.0 {
        for s in &mut out {
            *s *= PEAK / max;
        }
    }
    out
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
        let samples = (DURATION * f64::from(SAMPLE_RATE)) as usize;
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
        // The first sample is silent and the attack rises gradually.
        assert!(s[0].abs() < 1e-9);
        assert!(s[..20].iter().all(|x| x.abs() < 0.1));
        assert!(s.last().is_some_and(|x| x.abs() < 1e-9));
        // Still sounding a second in (the low note rings).
        let at = SAMPLE_RATE as usize;
        let ring = s[at..at + 2000].iter().fold(0.0_f64, |m, x| m.max(x.abs()));
        assert!(ring > 0.05, "ring {ring}");
    }
}
