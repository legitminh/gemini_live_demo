//! Orders text sent to Grok so a barge-in cannot be spoken over by audio
//! that was already in flight, and so the next sentence waits until Grok
//! finishes the current utterance.

use crate::grok::{text_clear, text_delta, text_done, GrokEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Queued {
    Delta(String),
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechAction {
    Send(String),
    Audio { bytes: Vec<u8>, epoch: u64 },
    Ended { epoch: u64 },
    Cleared { epoch: u64 },
    Failed(String),
}

#[derive(Debug)]
pub struct SpeechQueue {
    epoch: u64,
    /// Drop audio until Grok acknowledges `text.clear`.
    discard: bool,
    /// `text.done` was sent and `audio.done` has not arrived yet.
    awaiting_audio: bool,
    pending: Vec<Queued>,
    opened: bool,
    playback_epoch: Option<u64>,
}

impl Default for SpeechQueue {
    fn default() -> Self {
        Self {
            epoch: 1,
            discard: false,
            awaiting_audio: false,
            pending: Vec::new(),
            opened: false,
            playback_epoch: None,
        }
    }
}

impl SpeechQueue {
    pub fn is_busy(&self) -> bool {
        self.opened || self.awaiting_audio || self.discard || !self.pending.is_empty()
    }

    /// True the first time audio for this epoch is played.
    pub fn note_playback(&mut self, epoch: u64) -> bool {
        if self.playback_epoch == Some(epoch) {
            false
        } else {
            self.playback_epoch = Some(epoch);
            true
        }
    }

    pub fn speak(&mut self, text: String) -> Vec<SpeechAction> {
        if text.is_empty() {
            return Vec::new();
        }
        if self.discard || self.awaiting_audio {
            self.pending.push(Queued::Delta(text));
            return Vec::new();
        }
        self.opened = true;
        vec![SpeechAction::Send(text_delta(&text))]
    }

    pub fn finish(&mut self) -> Vec<SpeechAction> {
        if self.discard || self.awaiting_audio {
            if self
                .pending
                .iter()
                .any(|item| matches!(item, Queued::Delta(_)))
            {
                self.pending.push(Queued::Done);
            }
            return Vec::new();
        }
        if !self.opened {
            return Vec::new();
        }
        self.opened = false;
        self.awaiting_audio = true;
        vec![SpeechAction::Send(text_done())]
    }

    pub fn cancel(&mut self) -> Vec<SpeechAction> {
        if !self.is_busy() {
            return Vec::new();
        }
        self.pending.clear();
        self.opened = false;
        self.awaiting_audio = false;
        self.playback_epoch = None;
        self.discard = true;
        self.epoch = self.epoch.saturating_add(1);
        vec![
            SpeechAction::Send(text_clear()),
            SpeechAction::Cleared { epoch: self.epoch },
        ]
    }

    /// Give up waiting for `audio.clear` and speak anything queued behind it.
    pub fn force_ready(&mut self) -> Vec<SpeechAction> {
        if !self.discard && !self.awaiting_audio {
            return Vec::new();
        }
        self.awaiting_audio = false;
        self.flush_pending()
    }

    pub fn on_grok(&mut self, event: GrokEvent) -> Vec<SpeechAction> {
        match event {
            GrokEvent::Audio(bytes) => {
                if self.discard || bytes.is_empty() {
                    Vec::new()
                } else {
                    vec![SpeechAction::Audio {
                        bytes,
                        epoch: self.epoch,
                    }]
                }
            }
            GrokEvent::Done => {
                self.awaiting_audio = false;
                if self.discard {
                    self.flush_pending()
                } else {
                    let mut actions = vec![SpeechAction::Ended { epoch: self.epoch }];
                    actions.extend(self.flush_pending());
                    actions
                }
            }
            GrokEvent::Cleared => {
                self.awaiting_audio = false;
                self.flush_pending()
            }
            GrokEvent::Error(message) => {
                if self.discard {
                    self.awaiting_audio = false;
                    let mut actions = vec![SpeechAction::Failed(message)];
                    actions.extend(self.flush_pending());
                    actions
                } else {
                    vec![SpeechAction::Failed(message)]
                }
            }
            GrokEvent::Ignore => Vec::new(),
        }
    }

    fn flush_pending(&mut self) -> Vec<SpeechAction> {
        self.discard = false;
        let pending = std::mem::take(&mut self.pending);
        let mut actions = Vec::new();
        for item in pending {
            match item {
                Queued::Delta(text) => {
                    self.opened = true;
                    actions.push(SpeechAction::Send(text_delta(&text)));
                }
                Queued::Done => {
                    if self.opened {
                        self.opened = false;
                        self.awaiting_audio = true;
                        actions.push(SpeechAction::Send(text_done()));
                    }
                }
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_then_finish_sends_delta_and_done() {
        let mut queue = SpeechQueue::default();
        let speak = queue.speak("Hello".into());
        assert_eq!(speak, vec![SpeechAction::Send(text_delta("Hello"))]);
        assert_eq!(queue.finish(), vec![SpeechAction::Send(text_done())]);
        assert!(queue.is_busy());
        assert_eq!(
            queue.on_grok(GrokEvent::Audio(vec![9, 9])),
            vec![SpeechAction::Audio {
                bytes: vec![9, 9],
                epoch: 1
            }]
        );
        assert!(queue.note_playback(1));
        assert!(!queue.note_playback(1));
        assert_eq!(
            queue.on_grok(GrokEvent::Done),
            vec![SpeechAction::Ended { epoch: 1 }]
        );
        assert!(!queue.is_busy());
    }

    #[test]
    fn cancel_drops_inflight_audio_and_plays_the_next_line() {
        let mut queue = SpeechQueue::default();
        queue.speak("old reply".into());
        let cleared = queue.cancel();
        assert!(cleared.contains(&SpeechAction::Cleared { epoch: 2 }));
        assert_eq!(queue.speak("new reply".into()), Vec::<SpeechAction>::new());
        assert_eq!(
            queue.on_grok(GrokEvent::Audio(vec![1])),
            Vec::<SpeechAction>::new()
        );
        let resumed = queue.on_grok(GrokEvent::Cleared);
        assert_eq!(resumed, vec![SpeechAction::Send(text_delta("new reply"))]);
        assert_eq!(
            queue.on_grok(GrokEvent::Audio(vec![2])),
            vec![SpeechAction::Audio {
                bytes: vec![2],
                epoch: 2
            }]
        );
    }

    #[test]
    fn next_line_waits_until_the_current_utterance_finishes() {
        let mut queue = SpeechQueue::default();
        queue.speak("first".into());
        queue.finish();
        assert!(queue.speak("second".into()).is_empty());
        let actions = queue.on_grok(GrokEvent::Done);
        assert_eq!(
            actions,
            vec![
                SpeechAction::Ended { epoch: 1 },
                SpeechAction::Send(text_delta("second"))
            ]
        );
    }

    #[test]
    fn idle_cancel_is_a_no_op() {
        let mut queue = SpeechQueue::default();
        assert!(queue.cancel().is_empty());
        assert_eq!(queue.epoch, 1);
    }

    #[test]
    fn empty_finish_does_not_open_an_utterance() {
        let mut queue = SpeechQueue::default();
        assert!(queue.finish().is_empty());
    }
}
