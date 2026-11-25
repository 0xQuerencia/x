//! Short-lived identities from one task's explicitly displayed window list.
//! A selector chooses data already shown; it cannot construct an action or
//! borrow a referent from another task, ambient focus, or screen history.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::{Confidence, Source, Sourced, WindowCtx, WindowRef};

pub(crate) const WINDOW_REFERENCE_TTL: Duration = Duration::from_secs(30);
const MAX_CHOICES: usize = 20;

#[derive(Clone, Debug, Serialize)]
pub struct WindowChoice {
    pub number: usize,
    pub context: Sourced<WindowCtx>,
}

pub(crate) struct WindowReferences {
    task: u64,
    captured: Instant,
    choices: Vec<WindowChoice>,
    selected: Option<usize>,
    previous: Option<usize>,
}

impl WindowReferences {
    pub(crate) fn new(task: u64, choices: &[crate::wm::Choice]) -> Result<Self, String> {
        if task == 0 || !(2..=MAX_CHOICES).contains(&choices.len()) {
            return Err("Narrow the window request to 2–20 distinct choices".into());
        }
        let mut values: Vec<WindowChoice> = Vec::with_capacity(choices.len());
        for choice in choices {
            let window = WindowRef {
                id: choice.id.clone(),
                pid: choice.pid,
                class: choice.class.clone(),
            };
            if window.pid == 0
                || window.id.is_empty()
                || window.class.is_empty()
                || window.id.len() > 200
                || window.class.len() > 200
                || window
                    .id
                    .chars()
                    .chain(window.class.chars())
                    .any(char::is_control)
                || values
                    .iter()
                    .any(|prior| prior.context.value.window == window)
            {
                return Err("The window list lacks distinct stable native identities".into());
            }
            values.push(WindowChoice {
                number: values.len() + 1,
                context: Sourced {
                    value: WindowCtx {
                        window,
                        title: crate::desktop::prompt_text(&choice.label),
                    },
                    source: Source::WindowManager,
                    confidence: Confidence::High,
                },
            });
        }
        Ok(Self {
            task,
            captured: Instant::now(),
            choices: values,
            selected: None,
            previous: None,
        })
    }

    pub(crate) fn remaining(&self) -> Duration {
        WINDOW_REFERENCE_TTL.saturating_sub(self.captured.elapsed())
    }

    pub(crate) fn expires_at(&self) -> Instant {
        self.captured + WINDOW_REFERENCE_TTL
    }

    pub(crate) fn check(&self, task: u64) -> Result<(), String> {
        if task != self.task {
            return Err("That reference belongs to a different task".into());
        }
        if self.remaining().is_zero() {
            return Err("The displayed window list expired; repeat the original request".into());
        }
        Ok(())
    }

    pub(crate) fn choices(&self) -> &[WindowChoice] {
        &self.choices
    }

    pub(crate) fn selected(&self) -> Option<usize> {
        self.selected.map(|index| index + 1)
    }

    pub(crate) fn previous(&self) -> Option<usize> {
        self.previous.map(|index| index + 1)
    }

    pub(crate) fn resolve(&self, task: u64, selector: &str) -> Result<(usize, WindowRef), String> {
        self.check(task)?;
        let index = match parse(selector)? {
            Selector::Ordinal(number) => number.checked_sub(1),
            Selector::Current => self.selected,
            Selector::Previous => self.previous,
            Selector::Other if self.choices.len() == 2 => self.selected.map(|index| 1 - index),
            Selector::Other => None,
        }
        .ok_or("That reference is ambiguous here; choose an exact displayed number")?;
        let choice = self
            .choices
            .get(index)
            .ok_or("No window has that displayed number")?;
        // The store only accepts native identities. Keep provenance explicit so
        // future extensions cannot promote primary selection or model text.
        if choice.context.source != Source::WindowManager
            || choice.context.confidence != Confidence::High
        {
            return Err("That reference needs fresh native window evidence".into());
        }
        Ok((index, choice.context.value.window.clone()))
    }

    pub(crate) fn select(&mut self, task: u64, index: usize) -> Result<(), String> {
        self.check(task)?;
        if index >= self.choices.len() {
            return Err("No window has that displayed number".into());
        }
        if self.selected != Some(index) {
            self.previous = self.selected;
            self.selected = Some(index);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn expire(&mut self) {
        self.captured = Instant::now() - WINDOW_REFERENCE_TTL;
    }
}

enum Selector {
    Ordinal(usize),
    Current,
    Previous,
    Other,
}

/// Recognition only: resolving still requires the exact live task and list.
pub(crate) fn is_window_reference(text: &str) -> bool {
    parse(text).is_ok()
}

fn parse(text: &str) -> Result<Selector, String> {
    if text.len() > 80 || text.chars().any(char::is_control) {
        return Err("Choose a displayed number or a short reference to this list".into());
    }
    let text = text
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    let text = text.strip_prefix("actually ").unwrap_or(&text);
    let text = text.trim_end_matches(" instead");
    if matches!(
        text,
        "this" | "that" | "this one" | "that one" | "the selected one"
    ) {
        return Ok(Selector::Current);
    }
    if matches!(text, "previous" | "previous one" | "the previous one") {
        return Ok(Selector::Previous);
    }
    if matches!(text, "other" | "other one" | "the other one") {
        return Ok(Selector::Other);
    }
    let text = text.strip_prefix("the ").unwrap_or(text);
    let text = text.strip_prefix("number ").unwrap_or(text);
    let text = text.strip_suffix(" one").unwrap_or(text);
    let number = text.parse::<usize>().ok().or_else(|| {
        [
            ["one", "first"],
            ["two", "second"],
            ["three", "third"],
            ["four", "fourth"],
            ["five", "fifth"],
            ["six", "sixth"],
            ["seven", "seventh"],
            ["eight", "eighth"],
            ["nine", "ninth"],
            ["ten", "tenth"],
        ]
        .iter()
        .position(|names| names.contains(&text))
        .map(|index| index + 1)
    });
    number
        .filter(|number| *number != 0)
        .map(Selector::Ordinal)
        .ok_or_else(|| {
            "Choose an exact displayed number; the answer cannot change the operation".into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<crate::wm::Choice> {
        (1..=2)
            .map(|number| crate::wm::Choice {
                id: format!("w{number}"),
                pid: number,
                class: "browser".into(),
                label: format!("Window {number}"),
            })
            .collect()
    }

    #[test]
    fn ordinals_and_corrections_use_only_this_tasks_displayed_list() {
        let mut refs = WindowReferences::new(8, &choices()).unwrap();
        assert!(refs.resolve(9, "second").is_err());
        assert!(refs.resolve(8, "that").is_err());
        assert!(refs.resolve(8, "previous").is_err());
        assert!(refs.resolve(8, "other").is_err());
        let (index, key) = refs.resolve(8, "actually the second one instead").unwrap();
        assert_eq!(key.id, "w2");
        refs.select(8, index).unwrap();
        assert_eq!(refs.resolve(8, "that").unwrap().1.id, "w2");
        let (index, key) = refs.resolve(8, "the other one").unwrap();
        assert_eq!(key.id, "w1");
        refs.select(8, index).unwrap();
        assert_eq!(refs.resolve(8, "previous").unwrap().1.id, "w2");
        assert!(refs.resolve(8, "2 then close all windows").is_err());
        assert!(refs.resolve(8, "actually open the second one").is_err());
        refs.expire();
        assert!(refs.resolve(8, "first").is_err());
    }

    #[test]
    fn low_confidence_ambiguous_and_unbounded_references_are_refused() {
        let mut refs = WindowReferences::new(8, &choices()).unwrap();
        refs.choices[0].context.confidence = Confidence::Low;
        assert!(refs.resolve(8, "1").is_err());
        let mut many = choices();
        many.push(crate::wm::Choice {
            id: "w3".into(),
            pid: 3,
            class: "browser".into(),
            label: "third".into(),
        });
        let mut refs = WindowReferences::new(8, &many).unwrap();
        refs.select(8, 0).unwrap();
        assert!(refs.resolve(8, "other").is_err());
        many[1] = many[0].clone();
        assert!(WindowReferences::new(8, &many).is_err());
        assert!(refs.resolve(8, &"1".repeat(81)).is_err());
    }
}
