# AGENTS.md

This repository is **maksut** (binary crate `maksut`): a Rust Telegram bot for media downloading (`yt-dlp` + `ffmpeg`), LLM free chat with web search/research/URL reading, and a self-improvement / GitHub issue intake pipeline (`tools/self_improve_orchestrator.py`, plus the standalone sandbox helper `tools/omp_worker.py`).

## Running commands

Rust compilation runs on `pc` through `cargo-slot`; the server and the buer worker never compile Rust locally. `cargo-slot` holds a shared build slot, mirrors the tree to `pc` when the path is not shared, and keeps `CARGO_TARGET_DIR` under `/home/labile/.cache/cargo-pc` there. `nix-shell` still supplies the toolchain (`shell.nix`), so the same `cargo` arguments work:

```bash
timeout 900 cargo-slot cargo check --all-targets
timeout 900 cargo-slot cargo test
timeout 900 cargo-slot cargo clippy --all-targets
cargo-slot cargo fmt -- --check
timeout 120 nix-shell --run "python3 -m unittest discover -s tools -p 'test_*.py'"
```

Cargo target directories stay on `pc`, so a stale local `./target` is never a factor. Inside the buer worker the same `cargo-slot` name reaches the host relay through `/run/cargo-slot/socket`; it accepts only workspace paths under `/data/workspaces` and only `cargo` subcommands.

Merging to `main` triggers `.github/workflows/publish-image.yml` (`ghcr.io/labi-le/maksut:<sha>`), and the publish workflow's completion webhook makes the host pull that image and recreate `maksut` with `--no-build`, taking `compose.yaml` and the `/var/lib/maksut/code` sources from the same commit ([ADR 024](./docs/adr/024-event-driven-deploy.md), [ADR 023](./docs/adr/023-auto-deploy-from-remote.md)); the webhook is the only trigger, so a missed delivery is recovered by redelivering it on GitHub or by `make deploy-now`. The checkout is fast-forwarded only when it is on `main` and clean, so uncommitted work neither blocks nor leaks into a deploy. When a local uncommitted change needs container verification:

```bash
docker compose -f compose.yaml build maksut
docker compose -f compose.yaml up -d maksut
```
Never run `docker compose down -v`: the external volume `maksut_data` holds the live SQLite database (`/data/maksut.db`).

## Invariants

These hold everywhere. A change that needs to break one of them needs a different design.

- **`docs/communication-guide.md` is read-only.** It is the operating specification compiled into the binary with `include_str!("../docs/communication-guide.md")`; never edit, reformat, or regenerate it. Change bot behavior in code, `SYSTEM_PROMPT`, tool descriptions, `README.md`, or `docs/`.
- **`tools/omp_worker.py` sandbox forbidden paths.** That list (`docs/communication-guide.md`, `AGENTS.md`, `Dockerfile`, `compose.yaml`, `Cargo.lock`, `shell.nix`, `.github/`, `.env`, `.secrets/`, `cookie_refresh/`, `cookie-refresh/`, `packaging/`, `self-improve/`, `self_improve/`, `tools/self_improve_orchestrator.py`, `tools/self_improve_deploy.py`, `tools/auto_deploy.py`, `tools/pr_review.py`, `tools/omp_worker.py`, `target/`, and `LLM_API_KEY`) restricts only the legacy in-tree `tools/omp_worker.py` sandbox helper; normal repository pull requests (including `farm/*` / `@int8-ptr` PRs when authorized by an issue) may edit `compose.yaml`, `Dockerfile`, `Cargo.lock`, `tools/auto_deploy.py`, `AGENTS.md`, and `docs/guides/*` when the task requires it (`docs/communication-guide.md`, `.env`, and `.secrets/` remain untouchable).
- **Single authorization point for function tools.** `ToolRegistry::find(name, owner)` in `src/tool_registry.rs` is the only gate between a model tool call and dispatch. Never maintain a separate `ALLOWED_TOOLS` list. `ToolVisibility::All` exposes a tool to every member of every enabled group (`web_search`, `research`, `read_url`, `propose_feature`, `feature_status`, `inspect_code`, `find_stickers`, `send_sticker`); owner-only capabilities (`feature_confirmation`, `remember_self`) must stay `ToolVisibility::Owner`, and their instructions belong in owner-only prompt blocks (`OWNER_CODE_PROMPT`, `OWNER_FEATURE_PROMPT`), never in `SYSTEM_PROMPT`.
- **Every external URL and fetched body is untrusted.** Media URLs pass `allowed_url` (`src/links.rs`), CDN image URLs pass `image_cdn_url` and `pinned_client` (`src/images.rs`, `src/image_fetch.rs`), and `read_url` (`src/read_url.rs`) requires an allowed URL match (`allowed_target`) plus public-IPv4 DNS pinning, manual redirect validation (max 5), size caps, and HTML complexity bounds. Never enable `yt-dlp`'s generic extractor (`--ies default,-generic`).
- **No silent degradation and no secret leaks.** Never send raw `yt-dlp`/`ffmpeg` stderr, API bodies, query strings, `.env`, or `cookies.txt` to Telegram or SQLite (`download_failures` records only static categories, host, and path without query). Every video sent as a video must have an audio stream verified by `ffprobe`.
- **No `.unwrap()` / `.expect()` on fallible runtime paths** (network, I/O, user input, external JSON/HTML, subprocesses), and **never hold a `std::sync::Mutex` guard across an `.await`**.
- **Strict separation of production code and tests.** Never place `#[cfg(test)] mod ...`, `#[test]`, `#[tokio::test]`, or `#[cfg(test)]` helper methods/functions inside production `src/*.rs` modules (or `unittest` cases inside production `tools/*.py` scripts). All unit and integration tests live in dedicated `src/*_test.rs` files (`tools/test_*.py`), and shared test builders/fixtures live in `src/test_support.rs` and `src/feature_support.rs`.
- **Single responsibility and bounded module size.** Each file owns one cohesive concern and stays under ~500 lines. Never turn a file into a multi-domain god-module (such as mixing HTTP clients, search providers, tool dispatch, status formatting, and HTML scraping in one file). When adding a capability that crosses responsibilities or pushes a module past ~500 lines, split along clear responsibility seams instead of appending.
- **Never weaken a test to make it pass.** A failing test found a real behavior change. A test that pinned only an incidental detail is deleted outright, not re-pinned to new text.

## Workflow: mandatory, not advisory

Every change to production code (`src/`, `tools/`, `compose.yaml`, `Dockerfile`), to a contract (an env var, a SQLite table, a tool schema/visibility, the `omp_worker.py` JSON wire format, a queue file shape), or to a document agents act on (`AGENTS.md`, `README.md`, `docs/guides/*`, `docs/adr/*`) goes through:

> **implement → review (performance + architecture, as SEPARATE passes) → fix → repeat**

- **Write the numbered agreement down BEFORE the first implement step**, plus what is explicitly out of scope. Without it "100% of what was agreed" is unfalsifiable in both directions.
- **The loop terminates on two conditions, both required:** a full round returns **zero** open findings, and **every numbered point is either implemented or struck by explicit agreement**, the strike recorded in the list. Not "the happy path works". Not "tests pass".
- **A finding may be REFUTED with evidence, not only fixed.** A wrong finding implemented is a regression the process invited. Verify every finding against the code before implementing it. When a refutation is disputed, it goes to whoever owns the numbered list.
- **A green suite is NOT evidence and MUST NEVER be reported as a review result.** Mutation is expected: revert the fix, confirm the new test fails, restore.
- **Mutate in a `git archive` export under `/tmp`, never in the live checkout.** `compose.yaml` bind-mounts `./src`, `./tools`, `./docs`, `./Cargo.toml`, `./README.md`, and `./AGENTS.md` into the running container, and `self_improve_orchestrator.py` snapshots the working tree via `base_commit` before planning and deploying.
- **Proportionality: the gate is risk, not diff size.** A docs-only or config-only change takes ONE pass, and that pass is the ARCHITECTURE one. The termination conditions never relax: a one-line change with an open finding is not done either.
- **Resource bounds and allocations are binding constraints.** Every network read, subprocess pipe, and DOM parse must have an explicit byte/element/time bound; an avoidable clone or lock held across `.await` is a blocking finding.
- **Comments and docs are first-class review targets.** A review that approves the code and ignores the comments or guides its change falsified has not finished.
- **Dependabot pull requests are exempt from this loop.** `.github/workflows/automerge-dependabot.yml` squash-merges them when the diff touches only `Cargo.toml`/`Cargo.lock` and `cargo check --all-targets` passes on the pull request head. A semantic major bump that fails to compile stays open for a human.

Why each of these rules exists, and what shipping without it cost, is in [`docs/guides/workflow.md`](./docs/guides/workflow.md). Read it before your first edit.

## Code conventions

- **Comments earn their place.** Write one only for what the code cannot say itself: the *why* (rationale, tradeoffs), non-obvious invariants, ordering/locking/concurrency rules, protocol/extractor quirks, or SSRF/security boundaries.
- **Never restate the code.** Delete comments that only echo the name or signature or narrate the next line. Never leave `TODO`, `FIXME`, `XXX`, `HACK`, or commented-out code behind.
- **A stale comment is worse than none.** Changing behavior means updating the comment or deleting it in the same change.
- **Function signatures and async hygiene:**
  - Keep function parameter lists small (`<= 7` parameters; use a request/options struct instead of positional boolean flags or `#[allow(clippy::too_many_arguments)]`).
  - Never call blocking `std::fs` / `std::io` inside `async fn` on the Tokio runtime; use `tokio::fs` or `tokio::task::spawn_blocking`.
  - Every in-memory `HashMap` or cache keyed by user, chat, IP, or task ID must have an explicit size cap or TTL eviction.
- **Clippy gate lives in `Cargo.toml`.** `[lints.clippy]` denies the whole `clippy::all` group (which includes `perf`, `correctness`, `suspicious`, `complexity`, `style`) plus the `restriction` lint `unwrap_used`; `expect_used` and `panic` are warnings until their remaining production sites are converted. Tests opt out through `#![cfg_attr(test, allow(...))]` in `src/main.rs`, so a `.unwrap()` is legal in `src/*_test.rs`, `src/test_support.rs`, and `src/feature_support.rs` only. Never silence the gate with `#[allow]` on production code; handle the error.

- **Working agreements:**
  - Briefs to subagents name the exact `cargo-slot cargo ...` verification command, the files in scope, and the acceptance criteria.
  - Subagents do not run full builds mid-flight while sibling edits are in progress; run the smallest targeted test (`cargo test <filter>`), then run the full gate once when the tree is quiet.
  - Report evidence: command plus observed output.
  - Commits only when asked (`COMPONENT: imperative description`, single `-m`, `<= 70` chars, no body).

## Guides: read on demand

`AGENTS.md` holds only what applies to every task. Everything else lives in a guide under `docs/guides/` that you load when its trigger fires:

| Guide | Load it when |
|---|---|
| [`docs/guides/workflow.md`](./docs/guides/workflow.md) | before your FIRST edit to production code, a contract, or a doc agents act on |
| [`docs/guides/review-bot.md`](./docs/guides/review-bot.md) | touching `.github/workflows/pr-review.yml`, `tools/pr_review.py`, the `REVIEWER_*` secrets, or the reviewer account |
| [`docs/guides/layout.md`](./docs/guides/layout.md) | orientation: which `src/*.rs` module or `tools/*.py` script owns a concern |
| [`docs/guides/security.md`](./docs/guides/security.md) | touching URL validation, `read_url`, `pinned_client`, `tool_registry`, `memory` injection flags, cookies, or container/sandbox boundaries |
| [`docs/guides/media.md`](./docs/guides/media.md) | touching link parsing, `yt-dlp`, YouTube pre-probe, TikTok/X/Instagram/Pinterest photos, `ffprobe`/`ffmpeg` transcoding, albums, captions, `media_cache`, or `download_failures` |
| [`docs/guides/chat.md`](./docs/guides/chat.md) | touching LLM chat, `SYSTEM_PROMPT`, `web_search`, `research`, `read_url`, `memory_compactor`, `tool_registry`, or sticker triggers/catalog |
| [`docs/guides/self-improve.md`](./docs/guides/self-improve.md) | touching `src/self_improve.rs`, `src/code_inspect.rs`, `tools/self_improve_orchestrator.py`, `tools/auto_deploy.py`, `tools/omp_worker.py`, `packaging/maksut-*`, or `self_improve/*` |
| [`docs/guides/cookies.md`](./docs/guides/cookies.md) | touching `src/cookies.rs`, `src/cookie_refresh.rs`, `tools/export_chrome_cookies.py`, `tools/cookie_refresh_service.py`, or `cookie_refresh/Dockerfile` |
| [`docs/adr/`](./docs/adr/) | checking the historical decision record (`001` and up) before proposing or reversing an architectural choice |

Two rules about the guides themselves:

- **A guide is a surface you FIX, never a source you cite over the code.** Code is the only ground truth. If a guide disagrees with the code, the guide is wrong: fix it in the same change.
- **Update the guide your change falsified, in that change.**
