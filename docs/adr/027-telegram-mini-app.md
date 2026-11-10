# 2026-11-10 — Telegram Mini App for owner memory and settings

## Context

`bot_self_memory` is edited only through the owner-only `remember_self` tool, and
`config.toml` only by hand on the host. The owner asked for a Telegram Mini App
that edits both: a list of memory topics with fact/source/updated_at, an edit
and a delete action, plus a form over the mutable settings, opened from a chat
button over HTTPS. The proposal named `update_topic`/`delete_topic`, which do not
exist: `src/self_memory.rs` has `record_self_fact` (write, eviction, Qdrant
write-through) and a private fuzzy `delete_self_fact`.

The host already terminates TLS in nginx for the deploy hook ([ADR 024](./024-event-driven-deploy.md)),
and `compose.yaml` publishes no port for `maksut`.

## Decision

1. **Configuration.** Add a `[web]` section: `enabled` (default `false`),
   `listen` (default `0.0.0.0:8090`), `public_url` (optional `https` address
   Telegram opens), `auth_max_age_secs` (default 86 400, range 60-604 800) and
   `max_body_bytes` (default 16 KiB, range 1 KiB-1 MiB). Validation rejects an
   unparseable `listen`, a non-`https` or query-bearing `public_url`, and
   out-of-range bounds. The listener is plain HTTP; the container binds all
   interfaces inside its own bridge network, and the host publishes only
   `127.0.0.1:8090`, so nginx is the only reachable path.
2. **Server.** `axum` 0.8 with `http1`, `json`, `tokio` only. `src/web.rs` owns
   the router, state and startup; the server runs only when `web.enabled`, and
   a bind failure is logged and leaves the bot running without the Mini App.
   Every request is bounded by `web.max_body_bytes` and a fixed timeout.
3. **Authorization.** Only Telegram `initData` is accepted, in
   `Authorization: tma <initData>`. `src/web_auth.rs` verifies
   `HMAC-SHA256(key = HMAC-SHA256("WebAppData", bot_token), msg = data_check_string)`,
   compares the digest in constant time, requires a fresh `auth_date`, and
   requires `user.id == owner_id`. Missing/malformed/expired/forged payloads
   answer 401; a valid payload from another user answers 403. Static assets are
   public because the page must load before JavaScript can read `initData`; only
   `/api/*` is gated. `initData`, the bot token and the signing key are never
   logged.
4. **Memory API.** `GET /api/memory` returns at most `code.memory_max_rows`
   rows. `PUT /api/memory/{topic}` edits one existing row through
   `record_self_fact`, so validation, eviction and the Qdrant write-through stay
   in one place. `DELETE /api/memory/{topic}` deletes one code fact by exact
   topic through a new `delete_self_topic` that touches code facts only. Rows
   with an empty `source_url` (owner directives) answer 409: they are created
   and removed only through `remember_self`. If `LLM_API_KEY` is absent, the
   process has no `ChatConfig` and edits answer 409 instead of writing a row
   that the Qdrant mirror would miss.
5. **Settings API.** `GET /api/config` exposes a fixed whitelist of scalar keys
   with their effective value, whether an environment variable shadows the
   file, and the restart note. `PUT /api/config` accepts one `{key, value}`,
   refuses unknown keys, wrong value types and keys currently set by an
   environment variable, rewrites only that value through `toml_edit` so
   comments and layout survive, writes atomically with a `config.toml.bak`
   backup, and validates the candidate with the startup parser
   (`Config::parse` + env overrides + `validate`) before replacing the file.
   The running process keeps its installed `Config`: changes apply after a
   restart, and the response says so.
6. **Chat button.** Owner-only `/settings` in a private chat answers with an
   inline `web_app` button for `web.public_url`; with the section disabled or
   the URL absent it explains what to set. Other users and groups get nothing.
7. **Static UI.** One page, vanilla HTML/CSS/JS, embedded with `include_str!`
   from `src/web_assets/`: `Telegram.WebApp.expand()`, a memory tab with edit
   and delete, a settings tab over the whitelisted keys, and nothing else.
   Responses carry a strict CSP (`script-src 'self' https://telegram.org`,
   no inline script; inline style is allowed only because the Telegram Web SDK
   injects its iframe stylesheet), `nosniff`, `no-referrer` and `no-store`.
8. **Deployment and docs.** `compose.yaml` publishes `127.0.0.1:8090:8090`;
   README documents the nginx vhost, the `[web]` keys and `/settings`;
   `docs/guides/layout.md` and `docs/guides/security.md` gain the new modules
   and boundaries; `example.config.toml` gains the section.

## Out of scope

- Applying settings without a restart (the installed `Config` is a `OnceLock`
  read by ~300 call sites; a live layer is a separate change).
- Any role other than the owner; read-only access or group usage.
- Creating or editing owner directives (`source_url` empty) from the web UI.
- TLS termination inside the process.
- Changes to prompts, tool schemas, or `docs/communication-guide.md`.

## Limits

| Constant | Value |
|---|---:|
| `web.listen` | `0.0.0.0:8090` |
| `web.auth_max_age_secs` | 86 400 (range 60-604 800) |
| `web.max_body_bytes` | 16 KiB (range 1 KiB-1 MiB) |
| `GET /api/memory` rows | `code.memory_max_rows` (default 50) |
| request timeout | 10 s |

## Consequences

- The Mini App adds one inbound listener; with `web.enabled = false` nothing
  binds and the published port stays closed in effect.
- A valid `initData` captured within `auth_max_age_secs` is a bearer credential
  for the owner. Freshness, the constant-time compare and the owner check bound
  the window; TLS stays mandatory in front.
- Settings edits are files, not live state: the UI must say "after restart", and
  keys shadowed by environment variables are not editable.
- The UI never writes SQL: memory edits go through the same functions as the
  chat tool, so a web edit cannot diverge SQLite and Qdrant.
