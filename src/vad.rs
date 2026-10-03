//! Decides when a spoken turn is finished.
//!
//! Gemini Live answers after the audio it is receiving goes quiet. A live mic
//! never does that on its own: room noise keeps the stream "active" forever.
//! Speech is forwarded as-is. About a second of the following quiet audio is
//! forwarded too, so the stream does not gap, and then the mic is dropped
//! until the next sentence.

use std::time::Duration;

use tokio::time::Instant;

const SPEECH_RMS: f32 = 0.012;
/// Quiet audio still has to be forwarded. Dropping it opens a gap, and Gemini
/// then never closes the turn.
const QUIET_TAIL: usize = 16_000;
const HOLD: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicAction {
    /// Not part of a spoken turn.
    Drop,
    /// Send this microphone chunk.
    Forward,
    /// Send this chunk, then a block of digital silence, then ignore the mic.
    Close,
}

#[derive(Debug)]
enum Phase {
    Idle,
    Speaking { quiet_samples: usize },
    Holding { until: Instant },
}

#[derive(Debug)]
pub struct MicGate {
    phase: Phase,
}

impl Default for MicGate {
    fn default() -> Self {
        Self { phase: Phase::Idle }
    }
}

impl MicGate {
    pub fn observe(&mut self, pcm: &[u8], now: Instant) -> MicAction {
        let rms = pcm_rms(pcm);
        let samples = pcm.len() / 2;
        if let Phase::Holding { until } = self.phase {
            if now >= until && rms >= SPEECH_RMS {
                self.phase = Phase::Speaking { quiet_samples: 0 };
                return MicAction::Forward;
            }
            if now < until || rms < SPEECH_RMS {
                if now >= until {
                    self.phase = Phase::Idle;
                }
                return MicAction::Drop;
            }
        }
        if rms >= SPEECH_RMS {
            self.phase = Phase::Speaking { quiet_samples: 0 };
            return MicAction::Forward;
        }
        if let Phase::Speaking { quiet_samples } = &mut self.phase {
            *quiet_samples += samples;
            if *quiet_samples >= QUIET_TAIL {
                self.phase = Phase::Holding { until: now + HOLD };
                return MicAction::Close;
            }
            return MicAction::Forward;
        }
        MicAction::Drop
    }

    /// No further audio arrived after speech. Close the turn.
    pub fn end_now(&mut self, now: Instant) -> bool {
        if matches!(self.phase, Phase::Speaking { .. }) {
            self.phase = Phase::Holding { until: now + HOLD };
            true
        } else {
            false
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Speaking { .. } => Some(Instant::now() + Duration::from_millis(900)),
            Phase::Holding { .. } | Phase::Idle => None,
        }
    }
}

pub fn pcm_rms(pcm: &[u8]) -> f32 {
    let mut sum = 0.0f64;
    let mut count = 0.0f64;
    for sample in pcm.chunks_exact(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f64 / 32768.0;
        sum += value * value;
        count += 1.0;
    }
    if count == 0.0 {
        0.0
    } else {
        (sum / count).sqrt() as f32
    }
}

/// 800 ms of 16 kHz 16-bit silence.
pub fn silence_pcm() -> Vec<u8> {
    vec![0u8; 16_000 * 8 / 10 * 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize, amplitude: f32) -> Vec<u8> {
        let mut pcm = Vec::with_capacity(samples * 2);
        for index in 0..samples {
            let sample = (amplitude * (index as f32 * 0.4).sin() * 32767.0) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        pcm
    }

    #[test]
    fn silence_before_speech_is_not_forwarded() {
        let mut gate = MicGate::default();
        let now = Instant::now();
        assert_eq!(gate.observe(&tone(320, 0.0), now), MicAction::Drop);
    }

    #[test]
    fn quiet_tail_is_forwarded_then_the_turn_closes() {
        let mut gate = MicGate::default();
        let start = Instant::now();
        assert_eq!(gate.observe(&tone(640, 0.2), start), MicAction::Forward);
        let quiet = tone(16_000 - 640, 0.0);
        assert_eq!(gate.observe(&quiet, start), MicAction::Forward);
        assert_eq!(gate.observe(&tone(640, 0.0), start), MicAction::Close);
        assert_eq!(
            gate.observe(&tone(640, 0.0), start + Duration::from_millis(200)),
            MicAction::Drop
        );
    }

    #[test]
    fn ending_without_more_audio_closes_the_turn() {
        let mut gate = MicGate::default();
        let start = Instant::now();
        assert_eq!(gate.observe(&tone(640, 0.2), start), MicAction::Forward);
        assert!(gate.end_now(start));
        assert!(!gate.end_now(start));
    }

    #[test]
    fn a_second_sentence_after_the_hold_is_forwarded() {
        let mut gate = MicGate::default();
        let start = Instant::now();
        assert_eq!(gate.observe(&tone(640, 0.2), start), MicAction::Forward);
        assert!(gate.end_now(start));
        assert_eq!(
            gate.observe(&tone(640, 0.2), start + Duration::from_millis(200)),
            MicAction::Drop
        );
        assert_eq!(
            gate.observe(&tone(640, 0.2), start + HOLD + Duration::from_millis(10)),
            MicAction::Forward
        );
    }

    #[test]
    fn silence_burst_is_long_enough_to_end_a_turn() {
        let pcm = silence_pcm();
        assert_eq!(pcm.len() % 2, 0);
        assert!(pcm.iter().all(|byte| *byte == 0));
        assert!(pcm.len() >= 16_000 * 2 * 7 / 10);
    }
}
