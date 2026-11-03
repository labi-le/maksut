use crate::addressing::AddressKind;
use crate::prelude::*;
use crate::{parse_http_endpoint, RouterMode, RouterSettings};

/// One pre-model reply decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Do not answer; a closed followup window is cleared by the caller.
    Silent,
    /// Do not answer now, but keep listening for the next message.
    Wait,
    /// Continue into the heavy chat model.
    Reply,
}

/// Everything the decision may look at. No database reads: the router runs on
/// every candidate message and must stay O(1) plus one bounded HTTP call.
pub(crate) struct RouterInput<'a> {
    pub(crate) text: &'a str,
    pub(crate) address: AddressKind,
    pub(crate) followup: bool,
    pub(crate) stop: bool,
    pub(crate) private: bool,
    pub(crate) reply_context: Option<&'a str>,
}

#[derive(Clone, Default)]
pub(crate) struct Router {
    mode: RouterMode,
    jev: Option<crate::jev::JevClient>,
    jev_slots: Option<Arc<Semaphore>>,
}

impl Router {
    pub(crate) fn new(
        mode: RouterMode,
        jev: Option<crate::jev::JevClient>,
        jev_slots: Option<Arc<Semaphore>>,
    ) -> Self {
        Self {
            mode,
            jev,
            jev_slots,
        }
    }

    pub(crate) fn from_config(settings: &RouterSettings) -> Result<Self, String> {
        let (jev, jev_slots) = match settings.mode {
            RouterMode::Jev => {
                let key = env::var("JEV_API_KEY")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| "JEV_API_KEY is required when router.mode = jev".to_string())?;
                let url = parse_http_endpoint(&settings.jev_url, "router.jev_url")?;
                let client = crate::jev::JevClient::new(
                    url,
                    key,
                    settings.jev_model.clone(),
                    Duration::from_millis(settings.jev_timeout_ms),
                    settings.jev_max_response_bytes,
                    settings.jev_min_confidence,
                )?;
                (
                    Some(client),
                    Some(Arc::new(Semaphore::new(settings.jev_concurrency))),
                )
            }
            RouterMode::Off | RouterMode::Rules => (None, None),
        };
        Ok(Self::new(settings.mode, jev, jev_slots))
    }

    pub(crate) fn mode(&self) -> RouterMode {
        self.mode
    }

    /// Runs the decision and applies its side effect: silence closes the
    /// followup window, wait leaves it, reply continues into the model.
    pub(crate) async fn gate(
        &self,
        chat: &crate::ChatConfig,
        message: &Message,
        input: &crate::ResolvedChatInput,
    ) -> Decision {
        let decision = self
            .decide(&RouterInput {
                text: &input.prompt,
                address: input.address,
                followup: input.followup,
                stop: crate::chat_style::is_stop_followup(&input.prompt),
                private: message.chat.is_private(),
                reply_context: input.reply_context.as_deref(),
            })
            .await;
        if self.mode != RouterMode::Off {
            tracing::info!(
                chat_id = message.chat.id.0,
                msg_id = message.id.0,
                mode = ?self.mode,
                address = ?input.address,
                followup = input.followup,
                stop = crate::chat_style::is_stop_followup(&input.prompt),
                decision = ?decision,
                "router decision"
            );
        }
        if decision == Decision::Silent {
            chat.clear_followup(message.chat.id).await;
        }
        decision
    }

    pub(crate) async fn decide(&self, input: &RouterInput<'_>) -> Decision {
        if input.stop {
            // `off` keeps the historical path, where stop phrases are handled
            // after the model turn; the router modes close the window at once.
            return match self.mode {
                RouterMode::Off => Decision::Reply,
                RouterMode::Rules | RouterMode::Jev => Decision::Silent,
            };
        }
        if input.address == AddressKind::Explicit {
            return Decision::Reply;
        }
        // A name form with content is already an unconditional `Reply` below;
        // spending a jev round trip on it could only downgrade that answer.
        if input.address == AddressKind::Named && !input.text.trim().is_empty() {
            return Decision::Reply;
        }
        let decision = match self.mode {
            RouterMode::Off => Decision::Reply,
            RouterMode::Rules => decide_rules(input),
            RouterMode::Jev => {
                let slot = self.jev_slots.as_ref().map(|slots| slots.try_acquire());
                if matches!(&slot, Some(Err(_))) {
                    decide_rules(input)
                } else {
                    match &self.jev {
                        Some(client) => match client.decide(&jev_state(input)).await {
                            Some(decision) => decision,
                            None => decide_rules(input),
                        },
                        None => decide_rules(input),
                    }
                }
            }
        };
        // A router answer without text cannot be delivered as a chat reply.
        // `off` never lands here with empty text: media and sticker turns are
        // explicit and return above, and ambient candidates require text.
        if self.mode != RouterMode::Off
            && decision == Decision::Reply
            && input.text.trim().is_empty()
        {
            Decision::Silent
        } else {
            decision
        }
    }
}

/// The deterministic table. It never answers a message nobody addressed to the
/// bot, so enabling it cannot add replies where the bot was silent before.
pub(crate) fn decide_rules(input: &RouterInput<'_>) -> Decision {
    if input.stop {
        return Decision::Silent;
    }
    match input.address {
        AddressKind::Explicit => return Decision::Reply,
        AddressKind::Named if !input.text.trim().is_empty() => return Decision::Reply,
        AddressKind::Named => return Decision::Silent,
        AddressKind::None => {}
    }
    if !input.followup {
        return Decision::Silent;
    }
    let text = input.text.trim();
    if text.is_empty() {
        return Decision::Silent;
    }
    if expects_answer(text) {
        return Decision::Reply;
    }
    if is_short_reaction(text) {
        return Decision::Silent;
    }
    Decision::Wait
}

const QUESTION_WORDS: &[&str] = &[
    "что",
    "че",
    "чё",
    "чо",
    "кто",
    "как",
    "где",
    "когда",
    "почему",
    "зачем",
    "хули",
    "нахуя",
    "какой",
    "какая",
    "какие",
    "каков",
    "сколько",
    "скок",
    "куда",
    "откуда",
    "чей",
    "чья",
    "разве",
    "неужели",
];

const REQUEST_STEMS: &[&str] = &[
    "скаж",
    "расскаж",
    "найд",
    "покаж",
    "объясн",
    "провер",
    "сдела",
    "напиш",
    "перевед",
    "посчита",
    "помог",
    "пришл",
    "скин",
    "поищ",
    "обнов",
    "запус",
    "постав",
    "удал",
    "исправ",
    "добав",
    "сравн",
    "подскаж",
    "дай",
    "напомн",
    "нарису",
    "состав",
    "придум",
    "предлож",
    "попробу",
    "кинь",
    "глян",
];

/// Followup turns answer only when they look like a real request; a plain
/// remark keeps the window open instead of paying for a heavy completion.
fn expects_answer(text: &str) -> bool {
    if text.contains('?') {
        return true;
    }
    let lower = text.to_lowercase();
    lower
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| {
            QUESTION_WORDS.contains(&word)
                || REQUEST_STEMS.iter().any(|stem| word.starts_with(stem))
        })
}

const REACTIONS: &[&str] = &[
    "спасибо",
    "спс",
    "ахах",
    "ахаха",
    "хаха",
    "хахаха",
    "хех",
    "лол",
    "ржу",
    "смешно",
    "круто",
    "класс",
    "топ",
    "ок",
    "окей",
    "оки",
    "хорошо",
    "понял",
    "поняла",
    "ясно",
    "угу",
    "ага",
    "ну да",
    "да",
    "нет",
    "молодец",
    "браво",
    "зачёт",
    "зачет",
    "пиздато",
    "кайф",
    "норм",
    "нормально",
    "по факту",
    "база",
    "жиза",
    "бывает",
];

fn is_short_reaction(text: &str) -> bool {
    if text.chars().count() > 32 {
        return false;
    }
    let normalized = text
        .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != ' ')
        .to_lowercase();
    if normalized.trim().is_empty() {
        return text.chars().all(|ch| !ch.is_alphanumeric());
    }
    REACTIONS.contains(&normalized.as_str())
        || normalized.starts_with("ахах")
        || normalized.starts_with("хаха")
}

const MAX_STATE_CHARS: usize = 2000;

fn clamp_state(value: &str) -> String {
    value.chars().take(MAX_STATE_CHARS).collect()
}

fn jev_state(input: &RouterInput<'_>) -> String {
    let chat = if input.private {
        "личный"
    } else {
        "группа"
    };
    let address = match input.address {
        AddressKind::Explicit => "явное",
        AddressKind::Named => "по имени",
        AddressKind::None => "нет",
    };
    let followup = if input.followup { "да" } else { "нет" };
    let mut state = format!(
        "чат: {chat}\nобращение: {address}\nокно продолжения: {followup}\nсообщение: {}",
        clamp_state(input.text.trim())
    );
    if let Some(context) = input.reply_context {
        state.push_str(&format!("\nконтекст ответа: {}", clamp_state(context)));
    }
    state
}
