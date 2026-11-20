# 029: Chat media in `chat_media` and Garage, deduplicated by sha256

Date: 2026-11-20
Status: implemented

## Context

Chat memory stores text only: `src/chat_media.rs` downloads a picture into
process memory for a single vision turn under `chat.image_max_bytes`, and a media
message without a caption is logged as `[вложение]` with `FLAG_JUNK`. The bytes
of what participants actually posted are never kept, so a question about an
earlier picture has no path back to it.

Issue #113 asked for a memory of chat media: a `chat_media` binding table, the
file itself in Garage with the sha256 of its content as the object key, and a
retrieval path. The maintainer fixed the open decisions in the thread: keep blobs
forever, ingest every attachment of enabled chats, use `aws-sdk-s3`, and keep
`chat_media` as a binding only (`caption` duplicates `chat_messages.text`).

## Decision

### Tables

`chat_media_blobs` holds one row per distinct byte sequence:

```
chat_media_blobs(sha256 TEXT PRIMARY KEY, kind TEXT NOT NULL,
                 bytes INTEGER NOT NULL, created_at INTEGER NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 described_at INTEGER NOT NULL DEFAULT 0)
```

`chat_media` binds a message to that blob:

```
chat_media(chat_id INTEGER NOT NULL, message_id INTEGER NOT NULL,
           kind TEXT NOT NULL, file_unique_id TEXT NOT NULL,
           file_id TEXT NOT NULL, storage_key TEXT NOT NULL,
           created_at INTEGER NOT NULL,
           PRIMARY KEY (chat_id, message_id))
CREATE INDEX chat_media_blob ON chat_media(storage_key)
```

`storage_key` is the sha256 in lowercase hex and the only authoritative pointer
into Garage. `file_id` is kept for a future cheap Telegram re-send, but a bot
`file_id` can expire, so it never authorizes a lookup on its own and a failed
send is repaired by `getFile` plus `storage_key`.

Both tables are created by `init_media_store` (`src/media_store.rs`) from
`open_database`, like `media_cache`.

### Storage bound and retention

Blobs are kept forever: no TTL, no eviction, no orphan cleanup, and no reference
counting. This is a deliberate tradeoff against the `AGENTS.md` rule that every
cache has a cap or a TTL. The cap on this store is the size of the `maksut_data`
volume, and disk usage is monitored by the operator. Deduplication by content
address saves space only on repeated bytes; `chat_media` still grows by one row
per media message. Deleting rows is not part of this design: a blob without a
binding is a supported state, not corruption.

### Ingest

`handle_message` calls `media_ingest::spawn` after the message is recorded and
after the sender, private-chat, and command gates. `spawn` is cheap: it reads the
attachment metadata and either returns or starts a task, so the reply path never
waits for a download.

Gates, in order:

1. Garage is configured (`garage.endpoint` non-empty plus both credential
   variables). Otherwise ingest is off and the bot behaves exactly as before.
2. The message is not flagged `FLAG_SECRET` or `FLAG_INJECTION` (the
   `memory_guard` flags computed in `handle_message`). A screenshot with a
   password must not live in the object store forever.
3. The attachment fits `garage.max_bytes` (20 MiB, the Bot API `getFile` limit)
   and, for videos and animations, `garage.max_video_secs` (60 s). Both checks
   run on `file.size` and `duration` before any download, like the document path
   in `src/chat_doc.rs`.
4. For a group or supergroup, `group_enabled` still returns true when the task
   runs. The handler already skipped disabled chats for link downloads; a plain
   media message is recorded before that check, so ingest repeats it.

The chosen kinds are `photo` (largest size), `video`, `animation`, `document`,
`audio`, `voice`, and `video_note`, each with its MIME type or a per-kind
fallback. The download reuses `chat_media::download_raw_file`: the same bounded
client, the same byte cap, and the same `chat.image_concurrency` semaphore.
`garage.concurrency` bounds simultaneous ingest tasks, each holding at most one
20 MiB buffer.

A photo that is also a vision turn is fetched twice: once by `chat_media` for
the model and once by ingest. Sharing those bytes would tie the archive to the
reply path and make a slow upload delay an answer, so the duplicate download is
the accepted cost.

After the bytes arrive, `persist` computes the sha256, skips the upload when
`chat_media_blobs` already has that key, uploads to
`PUT {endpoint}/{bucket}/{sha256}` with a `garage.timeout_secs` deadline, inserts
the blob row, then the binding. A failed upload writes neither row: a binding
never points at a missing object. A failure after the upload but before the
binding leaves an orphan blob, which the retention rule above explicitly allows.

### Client

`src/garage.rs` wraps `aws-sdk-s3` (chosen by the maintainer over a hand-written
SigV4 signer). The client is built once per process in `Settings::from_config`,
uses path-style addressing (Garage serves buckets as path segments), a fixed
region, explicit credentials from `GARAGE_ACCESS_KEY_ID` and
`GARAGE_SECRET_ACCESS_KEY`, and `RequestChecksumCalculation::WhenRequired` so the
body stays a plain `Content-Length` request. Credentials never enter
`config.toml`, the returned `Config` value, or logs.

### Deployment

`compose.yaml` gains a `garage` service (`dxflrs/garage:v2.5.0`) with
`garage/garage.toml`, its data under the shared `maksut_data` volume, and
`GARAGE_RPC_SECRET`/`GARAGE_ADMIN_TOKEN` from the environment. The `maksut`
service receives `GARAGE_ENDPOINT`, `GARAGE_BUCKET`, `GARAGE_REGION` and the two
credential variables; all default to empty, so an existing deployment is
unaffected until the operator provisions the store:

```
garage layout assign -z dc1 -c 1G <node-id>   # node id from `garage status`
garage layout apply --version 1
garage bucket create maksut
garage key create maksut
garage bucket allow --read --write maksut --key maksut
```

Then set `GARAGE_ENDPOINT=http://garage:3900`, `GARAGE_ACCESS_KEY_ID` and
`GARAGE_SECRET_ACCESS_KEY` in `.env` and restart `maksut`.

## Bound table

| Bound | Setting | Default |
|---|---|---|
| Object size | `garage.max_bytes` | 20 MiB (`getFile` limit) |
| Video and animation duration | `garage.max_video_secs` | 60 s |
| Upload deadline | `garage.timeout_secs` | 20 s |
| Concurrent ingest tasks | `garage.concurrency` | 2 |
| Downloaded bytes in flight | `chat.image_concurrency` × `garage.max_bytes` | 4 × 20 MiB |

## Failure handling

- Download failure: one `tracing::warn` with the static stage and kind, no
  binding, no partial row. Attachment bytes never reach logs.
- Upload failure or timeout: static `upload` category, no rows written. The
  message is already recorded, so memory keeps the text; the media is simply
  absent.
- SQLite failure after a successful upload: static `store` category, orphan
  blob, consistent with the retention rule.
- Garage down while ingest runs: uploads fail per object; the bot keeps replying.
- Credentials missing: ingest is disabled at startup, logged as
  `garage = false` in `settings loaded`.

## Alternatives rejected

- **`caption` in `chat_media`.** `chat_messages.text` already holds the caption
  and is indexed for retrieval; a second copy is a divergence bug waiting to
  happen.
- **Eviction by reference counting.** The maintainer chose eternal storage, and a
  refcount adds a failure mode (a crash between count and delete) without
  reclaiming rows that are meant to stay.
- **Hand-written SigV4 over `reqwest`.** Fewer dependencies, but the maintainer
  chose `aws-sdk-s3`; the request shape is covered by
  `src/media_ingest_test.rs` against a local fake S3 endpoint.
- **Bytes in SQLite.** It would grow the live database volume and the backup in
  the same way, while losing the content-address deduplication across rows.

## Image descriptions, Qdrant indexing, and delivery

- **VLM image descriptions (`src/media_describe.rs`, `src/prompts/media_describe.md`):**
  When `garage.describe_model` (default `deepseek-v4.1-flash`, overridable by `GARAGE_DESCRIBE_MODEL`) is non-empty, `media_ingest` describes newly stored photos, stickers (or animated/video sticker thumbnails), image documents (`image/*`, up to `chat.image_max_bytes`), and video/animation/video-note thumbnails via `POST {llm.base_url}/chat/completions` (`max_tokens = 300`, `temperature = 0`, `reasoning_effort = "none"`), clamps the output to `garage.describe_max_chars` (default 400), records `llm_usage` under `KIND_MEDIA_DESCRIBE = "media_describe"`, checks `memory_guard::detect_flags` to withhold secret/injection descriptions, and writes `description` / `described_at` to `chat_media_blobs`. Because descriptions live on `chat_media_blobs`, identical bytes across messages and chats are described at most once. `backfill_blob_descriptions` in `chat_indexer` / `reindex_with` also lazily backfills undescribed `photo`/`sticker` blobs directly from Garage (`GarageClient::get`).
- **Qdrant `kind:"media"` indexing (`sync_media_points` and `sync_media_points_for_blob` in `src/media_describe.rs`):**
  Both post-description ingest (immediate per-blob upsert via `sync_media_points_for_blob` plus cursor advance via `sync_media_points`) and `chat_indexer` / `reindex_with` embed unflagged (`(m.flags & ~4) = 0`) media messages that have a non-empty blob description or caption (`compose_media_search_text`) and upsert points with id `point_id("media:{model}:{chat_id}:{m.id}")` and payload `kind = "media"`, `chat_id`, `message_id`, `telegram_message_id`, `media_kind`, `storage_key`, `text`, and `model`, advancing `qdrant_media_cursor:{identity}|{model}` in `memory_meta`.
- **Retrieval and delivery (`src/chat_search.rs`, `src/media_send.rs`, `src/memory.rs`):**
  `load_memory` and `chat_search` surface media messages with their VLM descriptions (`compose_media_search_text`), and `chat_search` queries Qdrant across `kind in ["chat", "media"]` (and includes blob descriptions in lexical fallback), attaching `telegram_message_id`, `media_kind`, `storage_key`, and `media_description` to matching rows that have a `chat_media` binding. The `send_chat_media` function tool (`ToolVisibility::All`, capped by `chat.send_chat_media_calls_per_reply` default 3) resolves a binding strictly within `ctx.chat_id` by `message_id` (internal or Telegram id) or `storage_key`, tries sending by cached Telegram `file_id` first, falls back to downloading the object from Garage (`GarageClient::get`) and refreshing `chat_media.file_id`, and records the sent message id and description in `chat_messages` so follow-up questions about the sent media have full context.
## Verification

`src/media_ingest_test.rs` covers: the largest photo size wins; a video above 60
seconds is rejected and a shorter one keeps its MIME type; oversized documents
and messages without attachments are skipped; a private chat candidate is not
treated as a group; the flag and store gates; two messages with identical bytes
produce one upload, one blob row, and two bindings; the request is
`PUT /maksut/<sha256>` with an `AWS4-HMAC-SHA256` authorization header and an
`x-amz-content-sha256` equal to the key; and a rejected upload writes no rows.

`src/media_store_test.rs` covers the schema round trip and the idempotent
`ON CONFLICT` behavior of both tables. `src/config_test.rs` covers the `[garage]`
bounds and the endpoint check. Removing the flag gate, the duration gate, or the
`blob_known` check makes those tests fail.
