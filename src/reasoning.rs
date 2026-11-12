use crate::jev::{ChoiceQuestion, JevClient};
use crate::prelude::*;
use crate::{is_stop_turn, LlmSettings, ReasoningSettings};

/// Resolved `reasoning_effort` tier. Ordered from cheapest to most expensive so
/// `min` implements the configured ceiling and `max` the omp-style floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Effort {
    None,
    Low,
    Medium,
    High,
}

impl Effort {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            _ => return None,
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// How `llm.reasoning_effort` reaches the request body. `Auto` asks the tiny
/// classifier per turn; the literal `auto` is never sent to the provider.
#[derive(Clone, Debug)]
pub(crate) enum ReasoningMode {
    Off,
    Fixed(String),
    Auto(ReasoningPolicy),
}

impl ReasoningMode {
    pub(crate) fn from_settings(settings: &LlmSettings) -> Self {
        let raw = settings
            .reasoning_effort
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
            Self::Off
        } else if raw.eq_ignore_ascii_case("auto") {
            Self::Auto(ReasoningPolicy::from_settings(&settings.reasoning))
        } else {
            Self::Fixed(raw.to_owned())
        }
    }

    pub(crate) fn is_configured(&self) -> bool {
        !matches!(self, Self::Off)
    }

    pub(crate) fn policy(&self) -> Option<&ReasoningPolicy> {
        match self {
            Self::Auto(policy) => Some(policy),
            Self::Off | Self::Fixed(_) => None,
        }
    }
}

/// The ceiling and the fallback tier of the `auto` mode, parsed once at
/// startup. `validate` rejects unknown tier names, so parse fallbacks here only
/// cover a hand-built config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReasoningPolicy {
    pub(crate) default: Effort,
    pub(crate) max: Effort,
}

impl ReasoningPolicy {
    pub(crate) fn from_settings(settings: &ReasoningSettings) -> Self {
        let tier = |value: &str, fallback: Effort| Effort::parse(value).unwrap_or(fallback);
        Self {
            default: tier(&settings.default, Effort::Low),
            max: tier(&settings.max, Effort::High),
        }
    }

    /// The classified tier, clamped the way omp clamps `auto`: never below
    /// `low` (a turn that asks for reasoning asks for at least some), never
    /// above the configured ceiling.
    pub(crate) fn resolve(&self, classified: Option<Effort>) -> Effort {
        classified
            .unwrap_or(self.default)
            .max(Effort::Low)
            .min(self.max)
    }
}

/// The per-turn `auto` classifier, shared through `Arc` so a turn can start it
/// before the model slot is taken without cloning the whole `ChatConfig`.
#[derive(Clone)]
pub(crate) struct ReasoningJudge {
    pub(crate) mode: ReasoningMode,
    pub(crate) classifier: Option<JevClient>,
    pub(crate) slots: Option<Arc<Semaphore>>,
}

impl ReasoningJudge {
    /// Builds the `auto` classifier once at startup. A missing `JEV_API_KEY`
    /// disables classification loudly and leaves the policy default in place, so
    /// a chat turn never depends on the tiny model being reachable; a malformed
    /// `[llm.reasoning]` section already fails startup validation.
    pub(crate) fn from_settings(settings: &LlmSettings) -> Self {
        let mode = ReasoningMode::from_settings(settings);
        let (classifier, slots) = Self::classifier(settings, &mode);
        Self {
            mode,
            classifier,
            slots,
        }
    }

    fn classifier(
        settings: &LlmSettings,
        mode: &ReasoningMode,
    ) -> (Option<JevClient>, Option<Arc<Semaphore>>) {
        if mode.policy().is_none() {
            return (None, None);
        }
        let key = env::var("JEV_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let Some(key) = key else {
            tracing::warn!(
                "llm.reasoning_effort = auto without JEV_API_KEY: using the fallback tier"
            );
            return (None, None);
        };
        let url = match crate::parse_http_endpoint(&settings.reasoning.url, "llm.reasoning.url") {
            Ok(url) => url,
            Err(error) => {
                tracing::warn!(%error, "reasoning classifier disabled");
                return (None, None);
            }
        };
        let policy = &settings.reasoning;
        match JevClient::new(
            url,
            key,
            policy.model.clone(),
            Duration::from_millis(policy.timeout_ms),
            policy.max_response_bytes,
            policy.min_confidence,
        ) {
            Ok(client) => (
                Some(client),
                Some(Arc::new(Semaphore::new(policy.concurrency))),
            ),
            Err(error) => {
                tracing::warn!(%error, "reasoning classifier disabled");
                (None, None)
            }
        }
    }

    /// Whether a classification could actually change this turn's tier. A fixed
    /// or disabled mode, a missing key and a pinned ceiling cannot; neither can
    /// a turn that never reaches the model: a suppressed stop-phrase, a vision
    /// turn or a blank prompt.
    pub(crate) fn wants(&self, prompt: &str, followup: bool, has_media: bool) -> bool {
        self.can_classify()
            && !has_media
            && !is_stop_turn(prompt, followup)
            && !prompt.trim().is_empty()
    }

    /// True when a classification could actually change the tier: a fixed or
    /// disabled mode, a missing key and a pinned ceiling all resolve to the same
    /// tier without a request.
    pub(crate) fn can_classify(&self) -> bool {
        self.classifier.is_some()
            && self
                .mode
                .policy()
                .is_some_and(|policy| policy.max > Effort::Low)
    }

    /// Resolves the tier for one turn from a prompt the caller already bounded
    /// with `clamp_state`, so the spawned task owns only the truncated copy.
    /// `None` means "not in auto mode"; a classifier failure or a busy slot
    /// falls back to the policy default and never fails the turn.
    pub(crate) async fn resolve(&self, prompt: &str) -> Option<Effort> {
        let policy = self.mode.policy()?;
        let Some(classifier) = &self.classifier else {
            return Some(policy.resolve(None));
        };
        // A ceiling at or below `low` pins every answer, so the request would
        // be pure cost.
        if policy.max <= Effort::Low {
            return Some(policy.resolve(None));
        }
        let classified = match &self.slots {
            Some(slots) => match slots.clone().try_acquire_owned() {
                Ok(_permit) => classify(classifier, prompt).await,
                Err(_) => None,
            },
            None => classify(classifier, prompt).await,
        };
        Some(policy.resolve(classified))
    }
}

/// The classifier's rubric: how much independent reasoning the answer needs,
/// ported from omp's `auto-thinking` question. Volume of work, politeness and
/// verbosity are explicitly not signals.
pub(crate) const EFFORT_INSTRUCTIONS: &str = "The state is one message a person sent to a chat bot. Judge how much independent reasoning the answer needs: whether the answer, fix, or design is given, or which explanation, design, or diagnosis remains open. Judge inherent difficulty, not politeness, tone, or verbosity. The volume of work never raises it. If torn between two levels, choose the lower one.";

pub(crate) const EFFORT_CRITERIA: &[(&str, &str)] = &[
    (
        "low",
        "One obvious answer or action: small talk, a definition, a translation, a count, a lookup, a short opinion.",
    ),
    (
        "medium",
        "A few candidates in one place, or one small trap: choosing between options, a short comparison, a small calculation, advice with tradeoffs.",
    ),
    (
        "high",
        "Several competing explanations or designs: architecture, debugging an unclear cause, proving a claim, a long multi-source analysis, a plan whose steps are not given.",
    ),
];

/// The classifier's question name inside the `answers` object.
pub(crate) const EFFORT_QUESTION: &str = "effort";

/// One bounded classification request against the configured tiny model.
/// `prompt` is bounded by `clamp_state` before it reaches this call.
pub(crate) async fn classify(classifier: &JevClient, prompt: &str) -> Option<Effort> {
    classifier
        .ask(
            prompt,
            ChoiceQuestion {
                name: EFFORT_QUESTION,
                instructions: EFFORT_INSTRUCTIONS,
                criteria: EFFORT_CRITERIA,
            },
        )
        .await
        .and_then(|choice| match Effort::parse(&choice) {
            Some(effort) => Some(effort),
            None => {
                tracing::warn!("effort classifier returned an unknown tier");
                None
            }
        })
}
