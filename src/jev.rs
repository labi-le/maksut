use crate::elapsed_ms;
use crate::prelude::*;
use crate::router::Decision;
use crate::sanitize_text;

/// One `choice` question of the TypeSafe System One (`Jev`) API: a name, the
/// task instruction, and the named answers with their criteria.
pub(crate) struct ChoiceQuestion<'a> {
    pub(crate) name: &'a str,
    pub(crate) instructions: &'a str,
    pub(crate) criteria: &'a [(&'a str, &'a str)],
}

/// The router's four-answer question. The reasoning classifier builds its own.
const ACTION: ChoiceQuestion<'static> = ChoiceQuestion {
    name: "action",
    instructions: crate::JEV_INSTRUCTIONS_PROMPT,
    criteria: &[
        ("reply", crate::JEV_REPLY_PROMPT),
        ("sticker", crate::JEV_STICKER_PROMPT),
        ("wait", crate::JEV_WAIT_PROMPT),
        ("silent", crate::JEV_SILENT_PROMPT),
    ],
};

/// Client for the TypeSafe System One (`Jev`) decisions API: one bounded
/// `choice` question per call. Any failure — transport, status, size, JSON
/// shape, low confidence — returns `None`, and the caller falls back to its
/// deterministic table or default tier.
#[derive(Clone)]
pub(crate) struct JevClient {
    http: reqwest::Client,
    url: Url,
    api_key: String,
    model: String,
    max_response_bytes: usize,
    min_confidence: f64,
}

impl JevClient {
    pub(crate) fn new(
        url: Url,
        api_key: String,
        model: String,
        timeout: Duration,
        max_response_bytes: usize,
        min_confidence: f64,
    ) -> Result<Self, String> {
        // The endpoint is fixed by configuration; a redirect must never turn
        // it into a request to an unexpected host carrying the API key.
        let telegram = &crate::config::config().telegram;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(telegram.connect_timeout_secs))
            .timeout(timeout)
            .pool_idle_timeout(Duration::from_secs(telegram.pool_idle_timeout_secs))
            .tcp_keepalive(Duration::from_secs(telegram.tcp_keepalive_secs))
            .tcp_nodelay(true)
            .redirect(Policy::none())
            .build()
            .map_err(|_| "unable to build the jev HTTP client".to_string())?;
        Ok(Self {
            http,
            url,
            api_key,
            model,
            max_response_bytes,
            min_confidence,
        })
    }

    /// Asks one question and returns the chosen label once it clears
    /// `min_confidence`. The body shape and the confidence ladder are shared by
    /// every caller so the router and the reasoning judge cannot drift.
    pub(crate) async fn ask(&self, state: &str, question: ChoiceQuestion<'_>) -> Option<String> {
        let started = time::Instant::now();
        let criteria: serde_json::Map<String, Value> = question
            .criteria
            .iter()
            .map(|(name, description)| {
                ((*name).to_owned(), Value::String((*description).to_owned()))
            })
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "state": state,
            "questions": {
                question.name: {
                    "type": "choice",
                    "instructions": question.instructions,
                    "criteria": criteria
                }
            }
        });
        let response = match self
            .http
            .post(self.url.clone())
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                tracing::warn!(
                    host = self.url.host_str(),
                    question = question.name,
                    elapsed_ms = elapsed_ms(started),
                    "jev request failed"
                );
                return None;
            }
        };
        if !response.status().is_success() {
            tracing::warn!(
                status = response.status().as_u16(),
                question = question.name,
                elapsed_ms = elapsed_ms(started),
                "jev HTTP error"
            );
            return None;
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            tracing::warn!(max = self.max_response_bytes, "jev response too large");
            return None;
        }
        // Decoded chunked bodies can exceed the advertised length or arrive
        // without one, so the cap is enforced while accumulating, not after.
        let mut response = response;
        let mut bytes = Vec::new();
        loop {
            let chunk = response.chunk().await.ok()?;
            let Some(chunk) = chunk else {
                break;
            };
            if bytes.len() + chunk.len() > self.max_response_bytes {
                tracing::warn!(max = self.max_response_bytes, "jev response too large");
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(bytes = bytes.len(), "jev response is not JSON");
                return None;
            }
        };
        let pointer = format!("/answers/{}", question.name);
        let answer = value.pointer(&pointer)?;
        let choice = answer.get("choice")?.as_str()?;
        let confidence = answer
            .get("probabilities")
            .and_then(|probabilities| probabilities.get(choice))
            .and_then(Value::as_f64)
            .or_else(|| answer.get("confidence").and_then(Value::as_f64))?;
        if !confidence.is_finite() || confidence < self.min_confidence {
            tracing::debug!(
                question = question.name,
                confidence,
                min = self.min_confidence,
                elapsed_ms = elapsed_ms(started),
                "jev confidence too low"
            );
            return None;
        }
        tracing::debug!(
            question = question.name,
            choice_len = choice.chars().count(),
            confidence,
            elapsed_ms = elapsed_ms(started),
            "jev answer"
        );
        Some(choice.to_owned())
    }

    /// The router's `reply` / `sticker` / `wait` / `silent` decision.
    pub(crate) async fn decide(&self, state: &str) -> Option<Decision> {
        let choice = self.ask(state, ACTION).await?;
        let decision = match choice.as_str() {
            "reply" => Some(Decision::Reply),
            "sticker" => Some(Decision::Sticker),
            "wait" => Some(Decision::Wait),
            "silent" => Some(Decision::Silent),
            _ => None,
        };
        match decision {
            Some(decision) => {
                tracing::debug!(decision = ?decision, "jev decision");
                Some(decision)
            }
            None => {
                tracing::warn!("jev returned an unknown action choice");
                tracing::trace!(choice = %sanitize_text(&choice), "jev raw choice");
                None
            }
        }
    }
}
