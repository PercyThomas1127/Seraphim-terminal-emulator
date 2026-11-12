//! `[[sound]]` rules and the audible bell: sounds played when events
//! arrive on the bus.

use std::time::{Duration, Instant};

use seraphim_events::{EventBus, Pattern, Subscription};
use seraphim_script::audio;

use crate::interaction::Rule;

struct SoundRule {
    rule: Rule,
    file: String,
    volume: f32,
    last: Option<Instant>,
}

pub struct Sounds {
    rules: Vec<SoundRule>,
    bell: String,
    volume: f32,
    last_bell: Option<Instant>,
    sub: Option<Subscription>,
}

/// The same sound plays at most this often.
const AGAIN: Duration = Duration::from_millis(150);

impl Sounds {
    pub fn new(
        audio: &seraphim_config::Audio,
        sounds: &[seraphim_config::Sound],
        bus: &EventBus,
    ) -> Sounds {
        let rules: Vec<SoundRule> = sounds
            .iter()
            .map(|s| SoundRule {
                rule: Rule::from_config(&seraphim_config::Rule {
                    event: s.event.clone(),
                    when: s.when.clone(),
                    ..Default::default()
                }),
                file: s.file.clone(),
                volume: s.volume.unwrap_or(audio.volume),
                last: None,
            })
            .collect();
        let mut names: Vec<String> = rules.iter().map(|r| r.rule.event.clone()).collect();
        if audio.bell != "none" {
            names.push("terminal.bell".into());
        }
        let sub = (!names.is_empty()).then(|| {
            bus.subscribe(Pattern::Any(names.iter().map(|n| Pattern::parse(n)).collect()), 256)
        });
        Sounds { rules, bell: audio.bell.clone(), volume: audio.volume, last_bell: None, sub }
    }

    /// Play the sounds for events that arrived. Returns how many played.
    pub fn pump(&mut self) -> usize {
        let Some(sub) = &self.sub else { return 0 };
        let now = Instant::now();
        let mut played = 0;
        for e in sub.drain() {
            if e.name == "terminal.bell"
                && self.bell != "none"
                && self.last_bell.is_none_or(|t| now - t >= Duration::from_millis(200))
            {
                self.last_bell = Some(now);
                played += audio::bell(&self.bell, self.volume) as usize;
            }
            for r in &mut self.rules {
                if !r.rule.matches(&e.name, &e.data) || r.last.is_some_and(|t| now - t < AGAIN) {
                    continue;
                }
                r.last = Some(now);
                let ok = match r.file.as_str() {
                    "bell" => audio::bell("default", r.volume),
                    "tone" => audio::bell("tone", r.volume),
                    f => audio::play(std::path::Path::new(f), r.volume),
                };
                played += ok as usize;
            }
        }
        played
    }
}
