use crate::prelude::*;
use crate::router::Decision;

/// Client for the TypeSafe System One (`Jev`) decisions API: one `choice`
/// question with three named answers. Any failure — transport, status, size,
/// JSON shape, low confidence — returns `None`, and the caller falls back to
/// the deterministic rules table.
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
        let http = reqwest::Client::builder()
            .timeout(timeout)
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

    pub(crate) async fn decide(&self, state: &str) -> Option<Decision> {
        let body = serde_json::json!({
            "model": self.model,
            "state": state,
            "questions": {
                "action": {
                    "type": "choice",
                    "instructions": "What should the Telegram bot do with this chat message?",
                    "criteria": {
                        "reply": "The message asks the bot something, addresses it by name, or clearly expects an answer.",
                        "wait": "The message addresses the bot but looks like the start of a longer thought; the next message will probably complete it.",
                        "silent": "No answer is needed: other people are talking, or this is a reaction, a joke, spam, or an empty message."
                    }
                }
            }
        });
        let response = self
            .http
            .post(self.url.clone())
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
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
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let answer = value.pointer("/answers/action")?;
        let choice = answer.get("choice")?.as_str()?;
        let confidence = answer
            .get("probabilities")
            .and_then(|probabilities| probabilities.get(choice))
            .and_then(Value::as_f64)
            .or_else(|| answer.get("confidence").and_then(Value::as_f64))?;
        if !confidence.is_finite() || confidence < self.min_confidence {
            return None;
        }
        match choice {
            "reply" => Some(Decision::Reply),
            "wait" => Some(Decision::Wait),
            "silent" => Some(Decision::Silent),
            _ => None,
        }
    }
}
