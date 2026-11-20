# 029: Chat media in `chat_media` and Garage, deduplicated by sha256

Date: 2026-11-20
Status: implemented (storage and ingest); retrieval, descriptions and delivery are
deferred to the decisions named below

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
                 bytes INTEGER NOT NULL, created_at INTEGER NOT NULL)
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

## Deferred

- **Image descriptions and `kind:"media"` vectors.** Description text needs a
  prompt file, which is maintainer-owned; the `chat_media_blobs.description` /
  `described_at` columns therefore do not exist yet. Without descriptions a
  media vector would duplicate the existing `kind:"chat"` vector for the
  caption, so the Qdrant side stays unchanged.
- **Retrieval and delivery.** Extending `chat_search` output or adding a send
  tool changes a tool contract that the maintainer owns. Until then the stored
  rows are written, not read: `storage_key` is the pointer a future read path
  will use.

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
