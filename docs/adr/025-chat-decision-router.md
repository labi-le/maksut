# 2026-11-03 — Pre-model reply decision router

## Context

Free chat already decides whether a message reaches the heavy model before any
completion runs: private chat, a reply to the bot, an `@username` mention, or a
bot name form (`src/chat_style.rs`), plus the `chat.followup_window_secs` window
after a delivered reply (`src/chat_client.rs`). Everything else in a group is
silent, and `PASS` silence is still decided by the heavy model for followup and
bare-attachment turns, so those turns pay a full completion to say nothing.

The owner asked for a separate cheap decision stage — "молчать / ждать /
ответить" — that runs before or instead of the heavy model: one deterministic
algorithm, and one that asks the TypeSafe System One (`Jev`) decisions API.

## Decision

- Add a `[router]` configuration section with `mode = "off" | "rules" | "jev"`
  (`off` by default), `jev_url`, `jev_model`, `jev_timeout_ms`,
  `jev_max_response_bytes`, `jev_min_confidence`. The API key is the
  `JEV_API_KEY` environment variable, like `LLM_API_KEY`; `mode = "jev"`
  without it fails startup instead of degrading silently.
- One decision type, `Decision::{Silent, Wait, Reply}` (`src/router.rs`), fed by
  `RouterInput` (prompt text, `AddressKind`, followup flag, stop phrase, chat
  kind, reply context). `src/chat_media.rs` classifies addressing once and may
  surface an unaddressed group message to the router when a router mode is on;
  `src/chat.rs` applies the decision before pacing, chat locks, the typing
  keepalive, and any media download, and logs a structured `tracing::info!`
  event (`router decision`) when `mode != "off"`. `Silent` closes the followup
  window, `Wait` leaves it as is, `Reply` continues into `chat_reply_with_proposal`.
- Rules table (`decide_rules`): stop phrase -> `Silent`; explicit address ->
  `Reply`; name form with content -> `Reply`; name form without content ->
  `Silent`; followup turn with a question mark, question word, or request stem
  -> `Reply`; short reaction (`ахаха`, `спасибо`, `ну да`, emoji-only) ->
  `Silent`; any other followup remark -> `Wait`; unaddressed message without a
  followup window -> `Silent`. A bare name form or a name plus punctuation
  carries no content and is classified unaddressed, so it follows the ordinary
  unaddressed/followup rules (silent outside the window, wait inside). The table
  never adds an answer on its own: the only `Reply` it produces is for an
  explicit address or a name form that already had content.
- Jev backend (`src/jev.rs`): `POST router.jev_url` with `model`, `state`
  (chat kind, addressing, followup flag, message text, reply context, each
  truncated to 2000 characters), and one `choice` question named `action` whose
  criteria are `reply`, `wait`, `silent`. The client requires the answer's
  `probabilities[choice]` (or `confidence`) to reach `router.jev_min_confidence`
  and bounds the response by `router.jev_max_response_bytes`, accumulating
  decoded chunks and aborting when the cap is crossed. `router.jev_concurrency`
  caps concurrent calls; a saturated router falls back to the rules table
  instead of queueing. Timeout, non-2xx, oversized body, malformed JSON,
  unknown choice, or low confidence fall back to the rules table; a
  low-confidence model answer never becomes a reply.
- Explicit addressing is sticky in every mode: private chat, a reply to the bot,
  and `@username` return `Reply` without an HTTP call, so the router can never
  make the bot ignore a direct mention. A stop phrase is the exception: in the
  router modes it returns `Silent` and closes the window. `mode = "off"` keeps
  the historical behavior, including the post-model stop-phrase handling.
- `Wait` vs `Silent` is the only state change: both skip the heavy model, and
  the difference is whether the followup window survives. The decision does not
  extend the window, so repeated `Wait` answers cannot keep a chat alive forever.

## Limits

| Constant | Value |
|---|---:|
| `router.jev_timeout_ms` | 800 (range 50-10 000) |
| `router.jev_max_response_bytes` | 16 KiB (range 1 KiB-1 MiB) |
| `router.jev_concurrency` | 8 (range 1-64) |
| `router.jev_min_confidence` | 0.6 (range 0-1) |
| `MAX_STATE_CHARS` | 2000 |

## Consequences

- With `mode = "off"` nothing changes: no new HTTP calls, no extra latency.
- With `mode = "rules"` the bot answers exactly where it did before, except that
  followup reactions and media turns are decided before the heavy model instead
  of by its `PASS` reply, and a bare name form inside an active window waits
  instead of paying for a completion.
- With `mode = "jev"` the model may wake the bot on an unaddressed group
  message; that is the intended recall gain and the reason it is not the
  default. A direct mention still always answers.
- The router state is the current message plus reply context, not chat history:
  persisting history for the router would add a database read on every group
  message. This keeps the decision O(1) plus one bounded HTTP call.
- With `mode = "jev"` the message text and reply context leave the process to
  `router.jev_url`; the operator choosing that mode accepts the same class of
  data exposure as sending the turn to the completion provider.
- The heavy model's prompts, tool schemas, and `docs/communication-guide.md`
  are unchanged.
