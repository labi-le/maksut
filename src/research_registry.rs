use crate::chat_text::truncate_text;
use crate::prelude::*;
use tokio::sync::{watch, Mutex};

/// Upper bound on the rows `research_status` returns in one call, so the tool
/// payload stays bounded even for the owner's `all_chats` view.
const SNAPSHOT_ROWS: usize = 20;
/// Question text is user input echoed back to the model; the snapshot keeps a
/// readable prefix instead of the whole 500-character question.
const QUESTION_CHARS: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResearchStatus {
    Running,
    Completed,
    Canceled,
    TimedOut,
}

impl ResearchStatus {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::TimedOut => "timed_out",
        }
    }

    const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Debug)]
pub(crate) struct ResearchSnapshot {
    pub(crate) id: String,
    pub(crate) user_id: UserId,
    pub(crate) question: String,
    pub(crate) status: ResearchStatus,
    pub(crate) completed_queries: usize,
    pub(crate) total_queries: usize,
    pub(crate) sources: usize,
    pub(crate) elapsed_ms: u128,
}

impl ResearchSnapshot {
    /// Tool-facing row; `elapsed_ms` is clamped to `u64` so the JSON number
    /// stays portable. Consumes the snapshot because the only caller drops it.
    pub(crate) fn into_json(self) -> Value {
        serde_json::json!({
            "id": self.id,
            "user_id": self.user_id.0,
            "question": self.question,
            "status": self.status.name(),
            "completed_queries": self.completed_queries,
            "total_queries": self.total_queries,
            "sources": self.sources,
            "elapsed_ms": u64::try_from(self.elapsed_ms).unwrap_or(u64::MAX),
        })
    }
}

struct Entry {
    id: String,
    chat_id: ChatId,
    user_id: UserId,
    question: String,
    started_at: time::Instant,
    finished_at: Option<time::Instant>,
    total_queries: usize,
    completed_queries: usize,
    sources: usize,
    status: ResearchStatus,
    cancel: watch::Sender<bool>,
}

impl Entry {
    fn snapshot(&self, now: time::Instant) -> ResearchSnapshot {
        let end = self.finished_at.unwrap_or(now);
        ResearchSnapshot {
            id: self.id.clone(),
            user_id: self.user_id,
            question: truncate_text(&self.question, QUESTION_CHARS),
            status: self.status,
            completed_queries: self.completed_queries,
            total_queries: self.total_queries,
            sources: self.sources,
            elapsed_ms: end.duration_since(self.started_at).as_millis(),
        }
    }
}

struct Inner {
    next_seq: u64,
    max_entries: usize,
    ttl: Duration,
    entries: Vec<Entry>,
}

impl Inner {
    /// Finished entries older than the TTL are dropped; running entries are
    /// never expired because their owner still holds the join handle.
    fn evict(&mut self, now: time::Instant) {
        let ttl = self.ttl;
        self.entries.retain(|entry| match entry.finished_at {
            Some(finished) => now.duration_since(finished) <= ttl,
            None => true,
        });
    }
}

/// Tracks running and recently finished research passes so the model can
/// inspect them with `research_status` and abort one with `research_cancel`.
/// The registry is the only shared state between concurrent turns of one
/// chat; the pass itself stays synchronous inside its own tool round.
pub(crate) struct ResearchRegistry {
    inner: Mutex<Inner>,
}

/// Handle for one running pass. Every path through `collect_research` calls
/// `finish`; a dropped handle (whole turn aborted) leaves the entry `running`
/// until the process restarts, because the TTL sweep only expires finished
/// entries.
pub(crate) struct ResearchJob<'a> {
    registry: &'a ResearchRegistry,
    id: String,
    cancel: watch::Receiver<bool>,
}

pub(crate) struct SnapshotFilter<'a> {
    pub(crate) chat_id: ChatId,
    pub(crate) all_chats: bool,
    pub(crate) owner: bool,
    pub(crate) research_id: Option<&'a str>,
}

pub(crate) struct CancelRequest<'a> {
    pub(crate) chat_id: ChatId,
    pub(crate) user_id: UserId,
    pub(crate) owner: bool,
    pub(crate) research_id: Option<&'a str>,
}

impl ResearchRegistry {
    pub(crate) fn from_config() -> Self {
        let research = &config().research;
        Self::new(
            research.max_tracked,
            Duration::from_secs(research.tracked_ttl_secs),
        )
    }

    pub(crate) fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_seq: 0,
                max_entries: max_entries.max(1),
                ttl,
                entries: Vec::new(),
            }),
        }
    }

    pub(crate) async fn start(
        &self,
        chat_id: ChatId,
        user_id: UserId,
        question: &str,
        total_queries: usize,
    ) -> Result<ResearchJob<'_>, String> {
        let now = time::Instant::now();
        let mut inner = self.inner.lock().await;
        inner.evict(now);
        while inner.entries.len() >= inner.max_entries {
            let Some(oldest) = inner
                .entries
                .iter()
                .position(|entry| entry.finished_at.is_some())
            else {
                return Err("слишком много активных ресерчей".into());
            };
            inner.entries.remove(oldest);
        }
        inner.next_seq += 1;
        let id = format!("res-{}", inner.next_seq);
        let (cancel, receiver) = watch::channel(false);
        inner.entries.push(Entry {
            id: id.clone(),
            chat_id,
            user_id,
            question: question.to_owned(),
            started_at: now,
            finished_at: None,
            total_queries,
            completed_queries: 0,
            sources: 0,
            status: ResearchStatus::Running,
            cancel,
        });
        Ok(ResearchJob {
            registry: self,
            id,
            cancel: receiver,
        })
    }

    pub(crate) async fn snapshots(
        &self,
        filter: SnapshotFilter<'_>,
    ) -> (Vec<ResearchSnapshot>, bool) {
        let now = time::Instant::now();
        let mut inner = self.inner.lock().await;
        inner.evict(now);
        let mut rows = Vec::new();
        let mut truncated = false;
        // The owner addressing one explicit id crosses chats, exactly like
        // `cancel`; an unfiltered listing still needs `all_chats`.
        let any_chat = filter.all_chats || (filter.owner && filter.research_id.is_some());
        for entry in inner.entries.iter().rev() {
            if !any_chat && entry.chat_id != filter.chat_id {
                continue;
            }
            if filter.research_id.is_some_and(|wanted| wanted != entry.id) {
                continue;
            }
            if rows.len() == SNAPSHOT_ROWS {
                truncated = true;
                break;
            }
            rows.push(entry.snapshot(now));
        }
        (rows, truncated)
    }

    /// Cancels one pass. A participant may cancel only a pass they started;
    /// the bot owner may cancel any pass they can see.
    pub(crate) async fn cancel(
        &self,
        request: CancelRequest<'_>,
    ) -> Result<ResearchSnapshot, String> {
        let now = time::Instant::now();
        let mut inner = self.inner.lock().await;
        inner.evict(now);
        let index = match request.research_id {
            Some(id) => inner.entries.iter().position(|entry| {
                entry.id == id && (request.owner || entry.chat_id == request.chat_id)
            }),
            // Without an id the caller targets their own running pass; the
            // owner targets the newest one in the chat.
            None => inner.entries.iter().rposition(|entry| {
                entry.status.is_running()
                    && entry.chat_id == request.chat_id
                    && (request.owner || entry.user_id == request.user_id)
            }),
        };
        let Some(entry) = index.and_then(|index| inner.entries.get_mut(index)) else {
            return Err(match request.research_id {
                Some(id) => format!("ресерч {id} не найден"),
                None => "нет активного ресерча".into(),
            });
        };
        if !request.owner && entry.user_id != request.user_id {
            return Err("чужой ресерч отменить нельзя".into());
        }
        if !entry.status.is_running() {
            return Err(format!(
                "ресерч {} уже завершён: {}",
                entry.id,
                entry.status.name()
            ));
        }
        entry.status = ResearchStatus::Canceled;
        entry.finished_at = Some(now);
        // A closed receiver means the pass already returned; the terminal
        // status it recorded stands and the signal is simply unobserved.
        let _ = entry.cancel.send(true);
        Ok(entry.snapshot(now))
    }
}

impl ResearchJob<'_> {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Separate receiver for `select!`: borrowing the job itself there would
    /// conflict with the progress update in the other branch.
    pub(crate) fn receiver(&self) -> watch::Receiver<bool> {
        self.cancel.clone()
    }

    pub(crate) async fn progress(&self, completed_queries: usize, sources: usize) {
        let mut inner = self.registry.inner.lock().await;
        if let Some(entry) = inner
            .entries
            .iter_mut()
            .find(|entry| entry.id == self.id && entry.status.is_running())
        {
            entry.completed_queries = completed_queries;
            entry.sources = sources;
        }
    }

    /// Records the terminal state unless `research_cancel` already did. The
    /// pass' own count wins over the value it wrote before the cancel signal.
    pub(crate) async fn finish(
        self,
        status: ResearchStatus,
        completed_queries: usize,
        sources: usize,
    ) {
        let now = time::Instant::now();
        let mut inner = self.registry.inner.lock().await;
        if let Some(entry) = inner.entries.iter_mut().find(|entry| entry.id == self.id) {
            entry.completed_queries = completed_queries;
            entry.sources = sources;
            if entry.status.is_running() {
                entry.status = status;
                entry.finished_at = Some(now);
            }
        }
    }
}
