//! Turns Gemini Live text into speech commands for Grok.
//!
//! Gemini's native live models speak audio. This demo discards that audio and
//! feeds the output transcript to Grok, so the voice the user hears is Grok's.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    InterimUser(String),
    FinalUser(String),
    AssistantFragment(String),
    GenerationComplete,
    TurnComplete,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Speak(String),
    FinishSpeech,
    CancelSpeech,
    User { text: String, is_final: bool },
    Assistant { text: String, is_final: bool },
}

#[derive(Debug, Default)]
pub struct TurnBridge {
    assistant: String,
    last_fragment: String,
    user_committed: String,
    user_interim: String,
    utterance_open: bool,
    user_sent_final: bool,
    assistant_sent_final: bool,
}

impl TurnBridge {
    pub fn handle(&mut self, signal: Signal) -> Vec<Effect> {
        match signal {
            Signal::InterimUser(text) => self.interim_user(&text),
            Signal::FinalUser(text) => self.final_user(&text),
            Signal::AssistantFragment(text) => self.assistant_fragment(&text),
            Signal::GenerationComplete => self.finish_speech(),
            Signal::TurnComplete => self.complete_turn(),
            Signal::Interrupted => self.interrupt(),
        }
    }

    fn interim_user(&mut self, text: &str) -> Vec<Effect> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        self.user_interim = text.to_string();
        self.user_sent_final = false;
        vec![Effect::User {
            text: self.user_display(),
            is_final: false,
        }]
    }

    fn final_user(&mut self, text: &str) -> Vec<Effect> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let interim = self.user_interim.trim().to_string();
        let interim_matches = !interim.is_empty()
            && (text.starts_with(interim.as_str())
                || interim.starts_with(text)
                || text.contains(interim.as_str()));
        if interim_matches
            || self.user_committed.is_empty()
            || text.starts_with(self.user_committed.as_str())
        {
            self.user_committed = text.to_string();
            self.user_interim.clear();
        } else if self.user_committed.ends_with(text) {
            self.user_interim.clear();
        } else {
            if needs_space(&self.user_committed, text) {
                self.user_committed.push(' ');
            }
            self.user_committed.push_str(text);
            self.user_interim.clear();
        }
        self.user_sent_final = true;
        vec![Effect::User {
            text: self.user_committed.clone(),
            is_final: true,
        }]
    }

    fn assistant_fragment(&mut self, fragment: &str) -> Vec<Effect> {
        if fragment.is_empty() {
            return Vec::new();
        }
        self.commit_user_draft();
        let delta = self.absorb_assistant(fragment);
        let mut effects = Vec::new();
        if !self.user_sent_final && !self.user_committed.is_empty() {
            self.user_sent_final = true;
            effects.push(Effect::User {
                text: self.user_committed.clone(),
                is_final: true,
            });
        }
        if !self.assistant.is_empty() {
            self.assistant_sent_final = false;
            effects.push(Effect::Assistant {
                text: self.assistant.clone(),
                is_final: false,
            });
        }
        let spoken = speakable(&delta);
        if !spoken.is_empty() {
            self.utterance_open = true;
            effects.push(Effect::Speak(spoken));
        }
        effects
    }

    fn absorb_assistant(&mut self, fragment: &str) -> String {
        if fragment == self.last_fragment {
            return String::new();
        }
        if fragment.starts_with(self.assistant.as_str())
            && fragment.len() > self.assistant.len()
            && cumulative_boundary(&self.assistant, fragment)
        {
            let delta = fragment[self.assistant.len()..].to_string();
            self.assistant = fragment.to_string();
            self.last_fragment = fragment.to_string();
            return delta;
        }
        self.assistant.push_str(fragment);
        self.last_fragment = fragment.to_string();
        fragment.to_string()
    }

    fn commit_user_draft(&mut self) {
        if self.user_committed.is_empty() && !self.user_interim.is_empty() {
            self.user_committed = std::mem::take(&mut self.user_interim);
        }
    }

    fn finish_speech(&mut self) -> Vec<Effect> {
        if !self.utterance_open {
            return Vec::new();
        }
        self.utterance_open = false;
        vec![Effect::FinishSpeech]
    }

    fn complete_turn(&mut self) -> Vec<Effect> {
        let mut effects = self.finish_speech();
        self.commit_user_draft();
        if !self.user_sent_final && !self.user_committed.is_empty() {
            effects.push(Effect::User {
                text: self.user_committed.clone(),
                is_final: true,
            });
        }
        if !self.assistant_sent_final && !self.assistant.is_empty() {
            effects.push(Effect::Assistant {
                text: self.assistant.clone(),
                is_final: true,
            });
        }
        self.reset_turn();
        effects
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        self.utterance_open = false;
        self.commit_user_draft();
        let mut effects = vec![Effect::CancelSpeech];
        if !self.user_sent_final && !self.user_committed.is_empty() {
            effects.push(Effect::User {
                text: self.user_committed.clone(),
                is_final: true,
            });
        }
        if !self.assistant.is_empty() {
            effects.push(Effect::Assistant {
                text: self.assistant.clone(),
                is_final: true,
            });
        }
        self.reset_turn();
        effects
    }

    fn reset_turn(&mut self) {
        self.assistant.clear();
        self.last_fragment.clear();
        self.user_committed.clear();
        self.user_interim.clear();
        self.utterance_open = false;
        self.user_sent_final = false;
        self.assistant_sent_final = false;
    }

    fn user_display(&self) -> String {
        if self.user_interim.is_empty() {
            self.user_committed.clone()
        } else if self.user_committed.is_empty()
            || self.user_interim.starts_with(self.user_committed.as_str())
        {
            self.user_interim.clone()
        } else {
            let mut combined = self.user_committed.clone();
            if needs_space(&combined, &self.user_interim) {
                combined.push(' ');
            }
            combined.push_str(&self.user_interim);
            combined
        }
    }
}

fn cumulative_boundary(current: &str, fragment: &str) -> bool {
    if current.is_empty() {
        return false;
    }
    let rest = &fragment[current.len()..];
    let boundary = current
        .chars()
        .last()
        .is_some_and(|ch| ch.is_whitespace() || is_soft_punct(ch))
        || rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_whitespace() || is_soft_punct(ch));
    boundary || current.len() >= 24
}

fn is_soft_punct(ch: char) -> bool {
    matches!(ch, '.' | ',' | '!' | '?' | ';' | ':' | '—' | '-' | '\'')
}

fn needs_space(left: &str, right: &str) -> bool {
    let left_end = left.chars().last();
    let right_start = right.chars().next();
    match (left_end, right_start) {
        (Some(a), Some(b)) => !a.is_whitespace() && !b.is_whitespace() && !is_soft_punct(b),
        _ => false,
    }
}

/// Strip markdown the model sometimes emits so Grok does not read symbols aloud.
pub fn speakable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '*' | '#' | '`' | '_' => {}
            '\n' | '\r' | '\t' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_text(effects: &[Effect]) -> Option<String> {
        effects.iter().rev().find_map(|effect| match effect {
            Effect::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
    }

    #[test]
    fn incremental_transcript_is_spoken_in_order() {
        let mut bridge = TurnBridge::default();
        let first = bridge.handle(Signal::AssistantFragment("Hello".into()));
        assert!(first.contains(&Effect::Speak("Hello".into())));
        let second = bridge.handle(Signal::AssistantFragment(" there.".into()));
        assert!(second.contains(&Effect::Speak(" there.".into())));
        assert_eq!(assistant_text(&second).as_deref(), Some("Hello there."));
        let done = bridge.handle(Signal::GenerationComplete);
        assert_eq!(done, vec![Effect::FinishSpeech]);
        let closed = bridge.handle(Signal::TurnComplete);
        assert!(closed.iter().any(|effect| {
            matches!(effect, Effect::Assistant { is_final: true, text } if text == "Hello there.")
        }));
        assert!(bridge.handle(Signal::TurnComplete).is_empty());
    }

    #[test]
    fn finish_is_sent_once() {
        let mut bridge = TurnBridge::default();
        bridge.handle(Signal::AssistantFragment("Hi.".into()));
        assert_eq!(
            bridge.handle(Signal::GenerationComplete),
            vec![Effect::FinishSpeech]
        );
        assert!(!bridge
            .handle(Signal::TurnComplete)
            .contains(&Effect::FinishSpeech));
    }

    #[test]
    fn cumulative_fragment_does_not_repeat_speech() {
        let mut bridge = TurnBridge::default();
        bridge.handle(Signal::AssistantFragment("Hello".into()));
        let effects = bridge.handle(Signal::AssistantFragment("Hello there.".into()));
        assert!(effects.contains(&Effect::Speak(" there.".into())));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::Speak(text) if text.contains("Hello"))));
    }

    #[test]
    fn duplicate_fragment_is_ignored() {
        let mut bridge = TurnBridge::default();
        bridge.handle(Signal::AssistantFragment("Hello".into()));
        let effects = bridge.handle(Signal::AssistantFragment("Hello".into()));
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::Speak(_))));
    }

    #[test]
    fn markdown_is_not_spoken() {
        assert_eq!(speakable("**Hello**\nthere"), "Hello there");
        let mut bridge = TurnBridge::default();
        let effects = bridge.handle(Signal::AssistantFragment("`code`".into()));
        assert!(effects.contains(&Effect::Speak("code".into())));
    }

    #[test]
    fn interrupt_cancels_speech_and_keeps_partial_text() {
        let mut bridge = TurnBridge::default();
        bridge.handle(Signal::InterimUser("what is".into()));
        bridge.handle(Signal::AssistantFragment("The answer is".into()));
        let effects = bridge.handle(Signal::Interrupted);
        assert!(effects.contains(&Effect::CancelSpeech));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::Assistant { is_final: true, text } if text == "The answer is"
        )));
        assert!(!effects.contains(&Effect::FinishSpeech));
    }

    #[test]
    fn interim_user_is_replaced_then_finalized() {
        let mut bridge = TurnBridge::default();
        let draft = bridge.handle(Signal::InterimUser("hel".into()));
        assert_eq!(
            draft,
            vec![Effect::User {
                text: "hel".into(),
                is_final: false
            }]
        );
        let revised = bridge.handle(Signal::InterimUser("hello there".into()));
        assert_eq!(
            revised,
            vec![Effect::User {
                text: "hello there".into(),
                is_final: false
            }]
        );
        let final_turn = bridge.handle(Signal::FinalUser("hello there".into()));
        assert_eq!(
            final_turn,
            vec![Effect::User {
                text: "hello there".into(),
                is_final: true
            }]
        );
    }

    #[test]
    fn word_prefix_is_appended_not_treated_as_cumulative() {
        let mut bridge = TurnBridge::default();
        bridge.handle(Signal::AssistantFragment("a".into()));
        let effects = bridge.handle(Signal::AssistantFragment("nd".into()));
        assert!(effects.contains(&Effect::Speak("nd".into())));
        assert_eq!(assistant_text(&effects).as_deref(), Some("and"));
    }

    #[test]
    fn empty_turn_does_not_open_speech() {
        let mut bridge = TurnBridge::default();
        assert!(bridge.handle(Signal::GenerationComplete).is_empty());
        assert!(bridge.handle(Signal::TurnComplete).is_empty());
    }
}
