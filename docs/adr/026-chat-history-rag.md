# 026: Chat history search over `chat_messages` embeddings

Date: 2026-11-03
Status: implemented
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

### Storage

`chat_embeddings` holds one row per indexed message:

| Column | Meaning |
|---|---|
| `chat_id`, `message_id` | `chat_messages.chat_id` and `chat_messages.id`; primary key |
| `chunk_text` | message text truncated to `memory.embedding_max_chars` |
| `vector` | normalized `f32` little-endian blob, so scoring is a dot product |
| `model` | embedding model that produced the vector |
| `created_at` | write time |

Vectors are unit-normalized on write. The model travels with every row: rows
written by another model are treated as foreign, so a model change reindexes
them instead of comparing incompatible dimensions.

### Embeddings

The call goes through the existing OpenAI-compatible endpoint
(`POST {llm.base_url}/embeddings`) with `memory.embedding_model`, reusing the
chat HTTP client, API key, and `config().retry.llm` policy: transport errors and
`retry.llm.retry_statuses` get exponential backoff inside one deadline, exactly
like `complete_with`. A provider-supplied `index` orders the result; every
vector must match `memory.embedding_dim` when it is nonzero. Setting
`memory.embedding_model = ""` disables the whole feature and both tools report
that plainly. The transport lives in `src/chat_embeddings.rs` (`embed_texts`),
retrieval in `src/chat_search.rs`, so `src/chat_client.rs` stays inside the
500-line module bound.

Local ONNX inference was the requested first choice. It is not in this change:
`fastembed`/`ort` would add a dependency (a `Cargo.lock` edit) and the
onnxruntime shared library plus model weights to the image (a `Dockerfile` and
`compose.yaml` edit). Both files are outside what an automated worker may touch,
so the local backend stays a maintainer change on top of the same storage and
scoring path.

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
- Work per worker tick: `memory.embedding_passes` batches.

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

## Alternatives rejected

- **Always-on semantic context.** It would grow every prompt and reintroduce the
  unbounded context the memory design removed. Retrieval is on demand.
- **`sqlite-vec` / an ANN index.** The image has no SQLite extension support
  (`sqlx` without extensions), and a brute-force pass over a bounded row count
  keeps the module dependency-free. Revisit if `search_scan_rows` starts
  dropping recent history.
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
