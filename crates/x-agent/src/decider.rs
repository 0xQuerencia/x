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
        crate::jev::send_with(ENDPOINT, &self.key()?, request)
    }
}

impl Planner for Decider {
    fn name(&self) -> String {
        self.model.label().into()
    }

    fn plan(&self, text: &str, smart: bool) -> Result<Reply, String> {
        crate::jev::plan_with(&|body| self.send(body), self.model.id(), text, smart)
    }

    fn decide_computer(
        &self,
        goal: &str,
        app: &str,
        observation: &crate::desktop::Observation,
        history: &[String],
    ) -> Option<Result<crate::computer::Step, String>> {
        Some(crate::jev_computer::decide(
            &|body| self.send(body),
            self.model.id(),
            goal,
            app,
            observation,
            history,
        ))
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
}
