//! Free Perplexity Decider 1.3 variants on OpenRouter's Decisions API.
//! Only chooses from locally validated actions. Never calls chat, paid models,
//! arbitrary commands, or a fallback model when provider quota is exhausted.

use std::path::PathBuf;

use serde_json::Value;

use crate::service::Planner;
use crate::tools::Reply;

const ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Model {
    Flash,
    Full,
}

impl Model {
    pub fn id(self) -> &'static str {
        match self {
            Self::Flash => "perplexity/pplx-decider-flash-v1.3:free",
            Self::Full => "perplexity/pplx-decider-v1.3:free",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Flash => "Perplexity Decider Flash V1.3 (free)",
            Self::Full => "Perplexity Decider V1.3 (free)",
        }
    }
}

pub struct Decider {
    pub model: Model,
    pub key_file: Option<PathBuf>,
    /// Separate opt-in for sending pixels, in addition to the app and remote grants.
    pub vision_file: Option<PathBuf>,
}

impl Decider {
    fn key(&self) -> Result<String, String> {
        let env = std::env::var("OPENROUTER_API_KEY").ok();
        let file = self
            .key_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        let key = env.or(file).unwrap_or_default().trim().to_string();
        if key.is_empty()
            || key.len() > 4096
            || !key
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '\\'))
        {
            return Err("Decider needs an OpenRouter key: set OPENROUTER_API_KEY or save openrouter-key in X's application data folder".into());
        }
        Ok(key)
    }

    fn send(&self, request: &Value) -> Result<Value, String> {
        crate::decision::send_with(ENDPOINT, &self.key()?, request)
    }

    fn wants_image(
        &self,
        goal: &str,
        observation: &crate::desktop::Observation,
        history: &[String],
        vision_allowed: bool,
    ) -> bool {
        if !vision_allowed
            || !self.vision_file.as_ref().is_some_and(|p| p.is_file())
            || !history.is_empty()
        {
            return false;
        }
        let lower = goal.to_ascii_lowercase();
        let visual_words = [
            "this button",
            "that button",
            "this icon",
            "that icon",
            "on screen",
            "on the screen",
            "look at",
            "visually",
            "screenshot",
            "what you see",
            "this part",
            "that part",
        ];
        visual_words.iter().any(|word| lower.contains(word)) || observation.elements.is_empty()
    }

    /// Injectable seams keep privacy tests off the actual desktop and network.
    fn send_visual_with(
        request: &Value,
        app: &str,
        title: &str,
        capture: &dyn Fn(&str, &str) -> Result<Vec<u8>, String>,
        send: &dyn Fn(&Value) -> Result<Value, String>,
    ) -> Result<Value, String> {
        let png = capture(app, title)?;
        let uri = format!(
            "data:image/png;base64,{}",
            crate::screenshots::base64_png(&png)
        );
        let mut body = request.clone();
        let state = std::mem::take(&mut body["state"]);
        body["state"] = serde_json::json!([
            state.to_string(),
            { "type": "image_url", "image_url": { "url": uri } }
        ]);
        send(&body)
    }

    fn decide_with(
        &self,
        goal: &str,
        app: &str,
        observation: &crate::desktop::Observation,
        history: &[String],
        vision_allowed: bool,
        capture: &dyn Fn(&str, &str) -> Result<Vec<u8>, String>,
        send: &dyn Fn(&Value) -> Result<Value, String>,
    ) -> Option<Result<crate::computer::Step, String>> {
        let image = self.wants_image(goal, observation, history, vision_allowed);
        Some(crate::decision_computer::decide(
            &|body| {
                if image {
                    Self::send_visual_with(body, app, &observation.title, capture, send)
                } else {
                    send(body)
                }
            },
            self.model.id(),
            goal,
            app,
            observation,
            history,
        ))
    }
}

impl Planner for Decider {
    fn name(&self) -> String {
        self.model.label().into()
    }

    fn plan(&self, text: &str, smart: bool) -> Result<Reply, String> {
        crate::decision::plan_with(&|body| self.send(body), self.model.id(), text, smart)
    }

    fn decide_computer(
        &self,
        goal: &str,
        app: &str,
        observation: &crate::desktop::Observation,
        history: &[String],
        vision_allowed: bool,
    ) -> Option<Result<crate::computer::Step, String>> {
        self.decide_with(
            goal,
            app,
            observation,
            history,
            vision_allowed,
            &crate::screenshots::capture_for_decider,
            &|body| self.send(body),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_free_models_and_decisions_endpoint_are_used() {
        assert_eq!(ENDPOINT, "https://openrouter.ai/api/alpha/decisions");
        for model in [Model::Flash, Model::Full] {
            assert!(model.id().starts_with("perplexity/pplx-decider"));
            assert!(model.id().ends_with(":free"));
        }
    }

    #[test]
    fn image_capture_requires_every_consent_gate_and_a_first_step() {
        use std::cell::Cell;

        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("vision");
        std::fs::write(&flag, b"on").unwrap();
        // A non-empty accessibility tree ensures only explicitly visual
        // wording asks for pixels; a missing tree is also a visual cue.
        let obs = crate::desktop::Observation {
            elements: vec![crate::desktop::Element {
                id: "e1".into(),
                reference: "@s:e1".into(),
                role: "button".into(),
                name: "Continue".into(),
                value: String::new(),
                actions: vec!["Click".into()],
            }],
            ..Default::default()
        };
        for vision in [false, true] {
            for remote in [false, true] {
                for granted in [false, true] {
                    for first in [false, true] {
                        for visual in [false, true] {
                            let p = Decider {
                                model: Model::Flash,
                                key_file: None,
                                vision_file: vision.then(|| flag.clone()),
                            };
                            let history = if first {
                                Vec::new()
                            } else {
                                vec!["previous step".to_string()]
                            };
                            let goal = if visual {
                                "click this button"
                            } else {
                                "click Continue"
                            };
                            let calls = Cell::new(0);
                            let capture = |_: &str, _: &str| {
                                calls.set(calls.get() + 1);
                                Ok(b"fake-png".to_vec())
                            };
                            let send = |_: &Value| {
                                Ok(serde_json::json!({
                                    "answers": {
                                        "operation": {
                                            "choice": "blocked",
                                            "probabilities": {"blocked": 0.99}
                                        }
                                    }
                                }))
                            };
                            p.decide_with(
                                goal,
                                "firefox",
                                &obs,
                                &history,
                                remote && granted,
                                &capture,
                                &send,
                            )
                            .unwrap()
                            .unwrap();
                            assert_eq!(
                                calls.get(),
                                u32::from(vision && remote && granted && first && visual),
                                "vision={vision} remote={remote} granted={granted} first={first} visual={visual}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn vision_is_opt_in_and_only_on_first_step() {
        let p = Decider {
            model: Model::Flash,
            key_file: None,
            vision_file: None,
        };
        let obs = crate::desktop::Observation::default();
        assert!(!p.wants_image("click this button", &obs, &[], true));
    }
}
