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
    /// Continue into the heavy chat model for a normal reply.
    Reply,
    /// Continue into the chat model in emotional sticker-reaction mode.
    Sticker,
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
            RouterMode::Off => (None, None),
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
        let stop = crate::chat_style::is_stop_followup(&input.prompt)
            && (input.followup || input.prompt.split_whitespace().count() <= 7);
        let decision = self
            .decide(&RouterInput {
                text: &input.prompt,
                address: input.address,
                followup: input.followup,
                stop,
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
                stop,
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
            // after the model turn; `jev` closes the window at once.
            return match self.mode {
                RouterMode::Off => Decision::Reply,
                RouterMode::Jev => Decision::Silent,
            };
        }
        let addressed = input.address == AddressKind::Explicit
            || (input.address == AddressKind::Named && !input.text.trim().is_empty());
        if self.mode == RouterMode::Off {
            return Decision::Reply;
        }
        let text = input.text.trim();
        // Direct questions, requests, long addressed messages, or captionless
        // addressed media/sticker turns (`text.is_empty()` with `Explicit`) go
        // straight to the model; short emotional addressed remarks consult Jev
        // so it can pick `Decision::Sticker` instead of a verbose text reply.
        if addressed && (text.is_empty() || expects_answer(text) || text.chars().count() > 140) {
            return Decision::Reply;
        }
        if text.is_empty() {
            return Decision::Silent;
        }
        let rules = decide_rules(input);
        if !addressed
            && input.followup
            && (input.reply_context.is_some() || addresses_other_participant(text))
        {
            return Decision::Wait;
        }
        if !addressed && input.followup && rules == Decision::Reply {
            return Decision::Reply;
        }
        let slot = self.jev_slots.as_ref().map(|slots| slots.try_acquire());
        let decision = if matches!(&slot, Some(Err(_))) {
            rules
        } else {
            match &self.jev {
                Some(client) => match client.decide(&jev_state(input)).await {
                    Some(Decision::Sticker) if !input.followup || !lower_has_action_verb(text) => {
                        Decision::Sticker
                    }
                    Some(Decision::Sticker) => Decision::Reply,
                    Some(_) if addressed => Decision::Reply,
                    Some(Decision::Silent) if input.followup && rules == Decision::Wait => {
                        Decision::Wait
                    }
                    Some(decision) => decision,
                    None => rules,
                },
                None => rules,
            }
        };
        tracing::debug!(
            mode = ?self.mode,
            decision = ?decision,
            followup = input.followup,
            address = ?input.address,
            text_len = input.text.chars().count(),
            "router decision"
        );
        decision
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
    if input.reply_context.is_some() {
        return Decision::Wait;
    }
    let text = input.text.trim();
    if text.is_empty() {
        return Decision::Silent;
    }
    if addresses_other_participant(text) {
        return Decision::Wait;
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
    "еще",
    "ещё",
    "алло",
    "ало",
    "але",
    "алё",
    "ау",
    "што",
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
    "оформ",
    "созда",
    "запиш",
    "сохран",
    "запомн",
    "забуд",
    "извин",
    "выбер",
    "прочит",
    "откро",
    "разбер",
    "поясн",
    "изуч",
    "посмотри",
    "чекн",
    "оцен",
    "проанализир",
    "проресерч",
    "заресерч",
    "ресерч",
    "ресерчн",
    "загугл",
    "погугл",
    "нагугл",
];

fn lower_has_action_verb(text: &str) -> bool {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| REQUEST_STEMS.iter().any(|stem| w.starts_with(stem)))
}

const OTHER_PARTICIPANT_VOCATIVES: &[&str] = &[
    "денис",
    "дениска",
    "вадик",
    "даня",
    "дань",
    "ваня",
    "вань",
    "ваньок",
    "ванёк",
    "влад",
    "владулик",
    "владик",
    "влудик",
    "кирилл",
    "кирюх",
    "андрей",
    "андрюх",
    "андрюха",
    "анрюх",
    "олег",
    "олежа",
    "иван",
];

fn addresses_other_participant(text: &str) -> bool {
    if text
        .split_whitespace()
        .any(|w| w.starts_with('@') && w.len() > 2)
    {
        return true;
    }
    let words: Vec<String> = text
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    let Some(first) = words.first() else {
        return false;
    };
    if OTHER_PARTICIPANT_VOCATIVES.contains(&first.as_str()) {
        return true;
    }
    words.len() >= 2
        && words
            .last()
            .is_some_and(|last| OTHER_PARTICIPANT_VOCATIVES.contains(&last.as_str()))
}

/// Followup turns answer only when they look like a real request; a plain
/// remark keeps the window open instead of paying for a heavy completion.
fn expects_answer(text: &str) -> bool {
    if text.contains('?') {
        return true;
    }
    let lower = text.to_lowercase();
    const FOLLOWUP_PREFIXES: &[&str] = &[
        "а если ",
        "а про ",
        "а по ",
        "а насчет ",
        "а насчёт ",
        "а без ",
        "а с ",
        "а не ",
        "а давай",
        "а может",
        "а щас",
        "а сейчас",
        "а теперь",
        "а тут",
        "а там",
        "а ты",
        "а эту",
        "а этот",
        "а это",
        "а вот ",
        "а другой",
        "а другая",
        "а другое",
        "а другие",
        "а еще ",
        "а ещё ",
        "и эту",
        "и этот",
        "и это",
        "и вот ",
        "вот эту",
        "вот этот",
        "не хочешь",
        "может ",
        "давай ",
    ];
    const FOLLOWUP_PHRASES: &[&str] = &[
        "твои мысли",
        "че думаешь",
        "чё думаешь",
        "что думаешь",
        "как думаешь",
        "а не лучше",
        "а не проще",
        "а не хочешь",
        "не лучше ли",
    ];
    if FOLLOWUP_PREFIXES.iter().any(|p| lower.starts_with(p))
        || FOLLOWUP_PHRASES.iter().any(|p| lower.contains(p))
    {
        return true;
    }
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
    "понял тебя",
    "поняла",
    "принял",
    "услышал",
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
    "по факту же",
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
