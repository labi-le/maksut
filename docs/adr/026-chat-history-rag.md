# 026: Chat history search over `chat_messages` embeddings

Date: 2026-11-03
Status: implemented; extended with the Qdrant index on 2026-11-07
Supersedes: the "no vector search" non-goal in
[`docs/superpowers/specs/2026-10-28-chat-memory-design.md`](../superpowers/specs/2026-10-28-chat-memory-design.md)

## Context

Chat memory keeps context bounded on purpose: `load_memory` returns the rolling
summary plus `memory.recent_messages` (default 60) of the newest unflagged rows,
and compaction folds older rows into the snapcompact archive. That design still
holds, but the raw log keeps growing, and a question about something said weeks
ago has no path into the model unless it happens to be in the summary. The
memory design called vector search unnecessary when the log was small; that
premise no longer matches the log size.

## Decision

Add a retrieval path over the raw log that never enlarges the always-on context:
a separate `chat_search` tool the model calls when it needs older messages.

### Storage and local embeddings

Vectors live exclusively in Qdrant (`qdrant/qdrant:v1.20.0`, collection
`memory.qdrant_collection`, default `maksut_memory`). SQLite holds only the raw
`chat_messages` log, `chat_summaries`, `bot_self_memory`, and the per-model
cursor `qdrant_chat_cursor:<model>` in `memory_meta`; `open_database` drops any
legacy `chat_embeddings` table so SQLite never stores vector blobs.

Embeddings are computed through `POST {embedding_url_or_llm_base}/embeddings`
(`memory.embedding_url`, env `EMBEDDING_URL`, falling back to `llm.base_url`
when empty) with `memory.embedding_model` (default
`intfloat/multilingual-e5-small`, `memory.embedding_dim = 384`). In
`compose.yaml`, the `embeddings` service runs HuggingFace
`text-embeddings-inference:cpu-1.8.3` locally on CPU (`1000:1000`,
`HUGGINGFACE_HUB_CACHE=/data/huggingface`), so vector indexing and semantic
search never depend on external API keys or quotas.

### Indexing

`chat_indexer` runs every `memory.embedding_interval_secs` (default 120). Each
pass selects at most `memory.embedding_batch` (default 32) unflagged non-empty
rows missing a vector for the configured model, oldest first, embeds them in one
call, and writes them in one transaction. `memory.embedding_passes` (default 3)
bounds one tick. A failed call leaves rows pending for the next tick; there is
no per-message spawn and no unbounded queue.

Indexing is both backfill and reindex: the scan predicate is "no vector for the
current model", so a changed model automatically becomes the backlog.

### Retrieval

`chat_search` takes `query`, optional `author_id`, optional `since`/`until`
(`YYYY-MM-DD`, MSK), and `limit`. It embeds the query, scores at most
`memory.search_scan_rows` (default 1000) stored vectors of the current chat,
drops scores below `memory.search_min_score` (default 0.25), deduplicates by
message id and text, and appends the newest `memory.search_recent_messages`
(default 5) unflagged messages of the same chat that are not already present.
Per row text is cut to `memory.search_chunk_chars` (default 400) and the whole
payload to `memory.search_max_chars` (default 4000). The tool answers with
data, not instructions, in the same shape as `find_stickers`.

### Visibility and authorization

`chat_search` is `ToolVisibility::All`, matching the issue: every participant of
the chat may search that chat. The chat id is never a tool argument. `run_tool_call`
passes the current turn's `chat_id` from `ToolContext`, so the query can only
read the chat it was asked in, regardless of what the model puts in the
arguments. `chat_index` is `ToolVisibility::Owner` because it spends embedding
calls and is maintenance, not conversation.

Both the indexer and the query require `chat_messages.flags = 0`. Messages
flagged `injection` or `secret` therefore stay out of retrieval exactly like
they stay out of `load_memory`, and a stray row written by an older version is
still filtered at query time.

## Bounds

- Rows scored per query: `memory.search_scan_rows`, 1000 by default. No vector
  cache is kept in memory, so there is no eviction policy to get wrong and no
  unbounded per-chat map.
- API response: `memory.embedding_max_response_bytes` (4 MiB default), separate
  from `llm.max_response_bytes` because a batch of 32 vectors is far larger than
  a chat completion.
- Inputs per call: `memory.embedding_batch`, each text cut to
  `memory.embedding_max_chars`.
- Output: `memory.search_chunk_chars` per row, `memory.search_max_chars` total,
  `chat.chat_search_calls_per_reply` (default 3) calls per reply.
- Work per worker tick: `memory.embedding_passes` batches of
  `memory.embedding_batch` rows upserted directly into Qdrant.
- Qdrant request: `memory.qdrant_timeout_secs` deadline and
  `memory.qdrant_max_response_bytes` response cap per call; self-fact ranking
  returns at most `memory.self_fact_limit` rows.

## Failure handling

- Embedding endpoint error: rows stay pending, the tick logs one warning and
  retries later. A search returns the endpoint error to the model wrapped as
  `chat_search: ...`, never as fabricated results.
- Dimension or count mismatch: nothing is written for that batch.
- Zero or non-finite vector: the batch is rejected rather than stored, because a
  normalized probability table needs a finite norm.
- Foreign model rows: re-embedded, never compared.
- Missing table rows or an empty chat: empty result set, which the tool reports
  as empty arrays.
- Qdrant down or rejecting: the indexer logs one warning and the tick retries;
  `chat_search` reports `degraded` and serves the SQLite semantic or lexical
  path; `remember_self` keeps the SQLite row and the backfill repairs the
  point later.

## Qdrant index (2026-11-07)

Qdrant becomes the ANN layer: `qdrant/qdrant:v1.20.0` joins `compose.yaml`
with REST 6333 / gRPC 6334 on the compose network and storage under the
existing `maksut_data` volume. This client uses the REST API only: a gRPC
client would add `tonic`/`prost` and a second HTTP stack for the same
upsert/query/delete operations, so the transport is `src/qdrant.rs` over the
existing `reqwest` stack.

Qdrant is the sole vector store; SQLite holds the canonical text rows:

- `sync_chat_points` selects unflagged `chat_messages` rows (`m.id > cursor_id`)
  in `memory.embedding_batch` (default 32) batches for up to
  `memory.embedding_passes` (default 3) passes per `chat_indexer` tick, embeds
  them, upserts the points into Qdrant, and advances `qdrant_chat_cursor:<model>`
  in `memory_meta` only after Qdrant acknowledges the write.
- A code fact (`source_url` non-empty) is embedded and upserted when
  `remember_self` writes it; a failed write is logged and repaired by the same
  tick's backfill. The backfill scrolls `kind = "self"` once and re-embeds a
  fact whose mirror is missing or whose payload `model`, `fact`, or
  `source_url` differs from SQLite. Owner directives (`source_url` empty) are
  never indexed and stay fully injected.
- Point ids are the first 63 bits of `SHA-256` over `chat:<chat_id>:<row_id>`
  or `self:<topic>`, so an upsert is idempotent and a delete needs no lookup.
- One collection (`memory.qdrant_collection`, default `maksut_memory`) with
  unnamed cosine vectors and payload indexes on `kind`, `chat_id`,
  `author_id`, `created_at`, `model`, `topic`, and `source_url`. An empty
  `memory.qdrant_url` (or `QDRANT_URL`, set by compose) disables the index and
  restores the SQLite-only path.

Retrieval order in `chat_search`: Qdrant query with mandatory `kind=chat`,
`chat_id`, and `model` filters plus the existing author/date filters, then the
SQLite dot-product scan, then lexical scoring. Every Qdrant hit is re-checked
against `chat_messages` for `flags = 0`, role/author, date bounds, and the
current Telegram message id, so a stale payload cannot bypass a filter. A
Qdrant or embedding error adds `degraded` to the tool payload so the model
sees the failure instead of a silent quality drop; an empty result above
`search_min_score` is a normal fallback and does not set it. Self-memory selection keeps every directive and ranks
code facts by `memory.self_fact_limit` (default 10); when the query or index
is unavailable it returns the full SQLite set, never a subset.

Every upsert, delete, and search is bounded by `memory.qdrant_timeout_secs`
and `memory.qdrant_max_response_bytes`. A dimension change needs a new
collection name because Qdrant refuses another vector size in an existing
collection; the collection and its indexes are created on first use and an
existing one is accepted. The `maksut_data` volume plus the idempotent
backfill is the backup story: dropping the collection loses nothing, because
SQLite re-derives chat vectors (stored) and self facts (re-embedded).

## Alternatives rejected

- **Always-on semantic context.** It would grow every prompt and reintroduce the
  unbounded context the memory design removed. Retrieval is on demand.
- **`sqlite-vec`.** The image has no SQLite extension support (`sqlx` without
  extensions). The Qdrant index above answers the ANN revisit trigger without
  an SQLite extension; with `memory.qdrant_url` empty the bounded brute-force
  scan stays.
- **Embedding the whole message text.** `chunk_text` is capped at
  `embedding_max_chars` like the summarizer input, so a single wall of text
  cannot dominate a batch or an API call.

## Verification

Covered in `src/chat_embeddings_test.rs`: indexing skips flagged rows, a query
never crosses chats, low scores are dropped, recent messages are merged and
deduplicated, author and date filters apply, foreign-model rows are re-embedded,
coverage is reported, malformed arguments are rejected, the transport retries a
transient status and does not retry a rejected request, and dispatch scopes the
tool to the current turn. Mutating the chat filter, the flags filter, or the
retry loop makes those tests fail.

`src/qdrant_test.rs` covers stable point ids, chat mirroring from stored
vectors without a second embedding call, re-sync of a rewritten vector through
the `(created_at, id)` cursor, self-fact upsert/delete ids, backfill of only
missing or changed facts plus stale-point deletion, Qdrant-first chat search
filters, rejection of a stale author payload, empty results not marked
degraded, explicit lexical fallback when the service is down, and ranked code
facts with fully injected owner directives.
