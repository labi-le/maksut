use crate::addressing::AddressKind;
use crate::prelude::*;
use crate::{parse_http_endpoint, ChatRouterSettings, RouterMode, RouterSettings};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Silent,
    Wait,
    Reply,
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
    pub(crate) forwarded: bool,
    pub(crate) reply_context: Option<&'a str>,
}
/// The jev client is built whenever its credentials exist, not only when the
/// global mode is `jev`: a single chat can turn the router on while every other
/// chat keeps the historical path.
#[derive(Clone, Default)]
pub(crate) struct Router {
    jev: Option<crate::jev::JevClient>,
    jev_slots: Option<Arc<Semaphore>>,
}

impl Router {
    pub(crate) fn new(
        jev: Option<crate::jev::JevClient>,
        jev_slots: Option<Arc<Semaphore>>,
    ) -> Self {
        Self { jev, jev_slots }
    }

    pub(crate) fn from_config(settings: &RouterSettings) -> Result<Self, String> {
        match Self::client(settings) {
            Ok(router) => Ok(router),
            // A globally enabled router that cannot run is an operator error.
            Err(reason) if settings.mode == RouterMode::Jev => Err(reason),
            Err(reason) => {
                tracing::warn!(
                    reason = %reason,
                    "per-chat jev router unavailable; JEV_API_KEY and router.jev_url enable it"
                );
                Ok(Self::default())
            }
        }
    }

    fn client(settings: &RouterSettings) -> Result<Self, String> {
        let (key, url) = jev_endpoint(settings)?;
        let client = crate::jev::JevClient::new(
            url,
            key,
            settings.jev_model.clone(),
            Duration::from_millis(settings.jev_timeout_ms),
            settings.jev_max_response_bytes,
        )?;
        Ok(Self::new(
            Some(client),
            Some(Arc::new(Semaphore::new(settings.jev_concurrency))),
        ))
    }

    pub(crate) async fn gate(
        &self,
        settings: ChatRouterSettings,
        chat: &crate::ChatConfig,
        message: &Message,
        input: &crate::ResolvedChatInput,
    ) -> Decision {
        let stop = crate::chat_style::is_stop_followup(&input.prompt)
            && (input.followup || input.prompt.split_whitespace().count() <= 7);
        let sender_id = message.from.as_ref().map(|u| u.id);
        let active_speaker = if settings.mode != RouterMode::Off && input.followup {
            chat.followup_speaker(message.chat.id).await
        } else {
            None
        };
        let cross_speaker_followup = settings.mode != RouterMode::Off
            && input.followup
            && input.address == AddressKind::None
            && active_speaker.is_some_and(|sp| sp.is_some() && sp != sender_id);
        let decision = if !stop
            && cross_speaker_followup
            && (input.reply_context.is_some()
                || addresses_other(&input.prompt)
                || !expects_answer(&input.prompt))
        {
            Decision::Wait
        } else if settings.mode != RouterMode::Off
            && !stop
            && input.address == AddressKind::None
            && is_short_reaction(&input.prompt)
        {
            Decision::Silent
        } else {
            self.decide(
                settings,
                &RouterInput {
                    text: &input.prompt,
                    address: input.address,
                    followup: input.followup,
                    stop,
                    private: message.chat.is_private(),
                    forwarded: message.forward_origin().is_some(),
                    reply_context: input.reply_context.as_deref(),
                },
            )
            .await
        };
        if settings.mode != RouterMode::Off {
            tracing::info!(
                chat_id = message.chat.id.0,
                msg_id = message.id.0,
                mode = ?settings.mode,
                address = ?input.address,
                followup = input.followup,
                stop,
                decision = ?decision,
                "router decision"
            );
        }
        let unaddressed_peer_turn = input.address == AddressKind::None
            && (input.reply_context.is_some() || addresses_other(&input.prompt));
        if decision == Decision::Silent || (decision == Decision::Wait && unaddressed_peer_turn) {
            chat.clear_followup(message.chat.id).await;
        }
        decision
    }

    pub(crate) async fn decide(
        &self,
        settings: ChatRouterSettings,
        input: &RouterInput<'_>,
    ) -> Decision {
        if input.stop {
            // `off` keeps the historical path, where stop phrases are handled
            // after the model turn; `jev` closes the window at once.
            return match settings.mode {
                RouterMode::Off => Decision::Reply,
                RouterMode::Jev => Decision::Silent,
            };
        }
        let addressed = input.address == AddressKind::Explicit
            || (input.address == AddressKind::Named && !input.text.trim().is_empty());
        if settings.mode == RouterMode::Off {
            return Decision::Reply;
        }
        let text = input.text.trim();
        if addressed && (text.is_empty() || expects_answer(text) || text.chars().count() > 140) {
            return Decision::Reply;
        }
        if text.is_empty() {
            return Decision::Silent;
        }
        let rules = decide_rules(input);
        if !addressed && input.followup && matches!(rules, Decision::Reply | Decision::Silent) {
            return rules;
        }
        if !addressed && input.followup && (input.reply_context.is_some() || addresses_other(text))
        {
            return Decision::Wait;
        }
        if !addressed && !input.followup && !can_wake_ambient(input, text) {
            return Decision::Silent;
        }
        let slot = self.jev_slots.as_ref().map(|slots| slots.try_acquire());
        let decision = if matches!(&slot, Some(Err(_))) {
            rules
        } else {
            match &self.jev {
                Some(client) => match client
                    .decide(&jev_state(input), settings.min_confidence)
                    .await
                {
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
            mode = ?settings.mode,
            decision = ?decision,
            followup = input.followup,
            address = ?input.address,
            text_len = input.text.chars().count(),
            "router decision"
        );
        decision
    }
}

/// Credentials the jev client needs, or the reason per-chat `jev` cannot be
/// enabled on this deployment.
fn jev_endpoint(settings: &RouterSettings) -> Result<(String, Url), String> {
    let key = env::var("JEV_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "JEV_API_KEY is required for the jev router".to_string())?;
    let url = parse_http_endpoint(&settings.jev_url, "router.jev_url")?;
    Ok((key, url))
}

/// Whether a per-chat `router_mode = "jev"` can work right now. The owner tool
/// and the Mini App refuse to store a mode that would only fall back to the
/// rules table.
pub(crate) fn jev_available() -> bool {
    jev_endpoint(&config().router).is_ok()
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
    if text.is_empty() || is_short_reaction(text) {
        return Decision::Silent;
    }
    if input.reply_context.is_some() || addresses_other(text) {
        return Decision::Wait;
    }
    if expects_answer(text) {
        Decision::Reply
    } else {
        Decision::Wait
    }
}

const QUESTION_WORDS: &str = "что че чё чо кто как где когда почему зачем хули нахуя \
    какой какая какие каков сколько скок куда откуда чей чья разве неужели еще ещё \
    алло ало але алё ау што";

const REQUEST_STEMS: &str = "скаж расскаж найд покаж объясн провер сдела напиш перевед \
    посчита помог пришл скин поищ обнов запус постав удал исправ добав сравн cравн подскаж \
    дай напомн нарису состав придум предлож попробу кинь глян оформ созда запиш сохран запомн \
    забуд извин выбер прочит откро разбер поясн изуч посмотри чекн оцен проанализир \
    проресерч заресерч ресерч ресерчн загугл погугл нагугл";

fn is_request_verb(w: &str) -> bool {
    let Some(tail) = REQUEST_STEMS
        .split_ascii_whitespace()
        .find_map(|stem| w.strip_prefix(stem))
    else {
        return false;
    };
    const NON_REQUEST_SUFFIXES: &str = "ал ала ало али ял яла яло яли ил ила ило или ул ула уло \
        ули ел ела ело ели аюсь яюсь уюсь юсь тся ться лся лась лось лись";
    !(tail.starts_with('л')
        || NON_REQUEST_SUFFIXES
            .split_ascii_whitespace()
            .any(|s| tail.ends_with(s)))
}

fn lower_has_action_verb(text: &str) -> bool {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(is_request_verb)
}

fn addresses_other(text: &str) -> bool {
    crate::addressing::addresses_other_participant(text, expects_answer(text))
}

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
    has_question_or_request(&lower)
}

fn has_question_or_request(lower: &str) -> bool {
    lower.contains('?')
        || lower
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .any(|word| {
                QUESTION_WORDS.split_ascii_whitespace().any(|q| q == word) || is_request_verb(word)
            })
}

fn can_wake_ambient(input: &RouterInput<'_>, text: &str) -> bool {
    let l = text.to_lowercase();
    input.reply_context.is_none()
        && !input.forwarded
        && !addresses_other(text)
        && !is_short_reaction(text)
        && !l.starts_with("[переслано]")
        && (has_question_or_request(&l)
            || l.split(|c: char| !c.is_alphanumeric())
                .any(|w| ["бот", "нейронк", "иишк"].iter().any(|s| w.starts_with(s))))
}

const REACTIONS: &[&str] = &[
    "спасибо",
    "спс",
    "сяп",
    "благодарю",
    "от души",
    "не в бровь а в глаз",
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
    "понятно",
    "ясненько",
    "принял",
    "услышал",
    "ясно",
    "ладно",
    "лан",
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
    "все верно",
    "всё верно",
    "логично",
    "согласен",
    "согласна",
    "именно",
    "точно",
    "бля",
    "блять",
    "блядь",
    "пизда",
    "пиздец",
    "пиздос",
    "сука",
    "ебать",
    "ахуеть",
    "охуеть",
    "нихуя",
    "нихера",
    "жесть",
    "капец",
    "треш",
    "трэш",
    "кринж",
];
fn is_short_reaction(text: &str) -> bool {
    if text.chars().count() > 48 {
        return false;
    }
    let lower = text.to_lowercase();
    let (mut normalized, mut first, mut count) = (String::with_capacity(lower.len()), "", 0usize);
    for word in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if count > 0 {
            normalized.push(' ');
        } else {
            first = word;
        }
        normalized.push_str(word);
        count += 1;
    }
    if count == 0 {
        return text.chars().all(|ch| !ch.is_alphanumeric());
    }
    REACTIONS.contains(&normalized.as_str())
        || normalized.starts_with("ахах")
        || normalized.starts_with("хаха")
        || (count <= 5 && matches!(first, "спасибо" | "благодарю" | "спс") && !expects_answer(text))
}
const MAX_STATE_CHARS: usize = 2000;

/// Bounds what the tiny classifier sees; the router and the reasoning judge
/// share one cap so an enormous message cannot inflate a classification call.
pub(crate) fn clamp_state(value: &str) -> String {
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
    let msg = clamp_state(input.text.trim());
    let mut state = format!(
        "чат: {chat}\nобращение: {address}\nокно продолжения: {followup}\nсообщение: {msg}"
    );
    if let Some(context) = input.reply_context {
        state.push_str(&format!("\nконтекст ответа: {}", clamp_state(context)));
    }
    state
}
