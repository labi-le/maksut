# Feature request intake and GitHub issue orchestrator

> **When to read this:** Read before touching `src/self_improve.rs`, `src/improve_store.rs`, `src/improve_views.rs`, `src/improve_commands.rs`, `src/code_inspect.rs`, `tools/self_improve_orchestrator.py`, `tools/auto_deploy.py`, `.github/workflows/publish-image.yml`, or `packaging/maksut-*`.

## Architecture: Telegram feature intake -> GitHub issue (`roboomp`)

The self-improvement intake is split across two isolated processes so the Telegram bot never executes shell or `gh` commands directly:

1. **Bot (`src/self_improve.rs`, inside the `maksut` container):** writes feature requests (wishlist items) to `self-improve/inbox/*.json`, reads status from `self-improve/outbox/*.json`, sends notifications (`improvement_notifier` polling every 5s and tracking `notified/`), and writes owner decisions to `approvals/` and `rejections/`.
2. **Host orchestrator (`tools/self_improve_orchestrator.py`, systemd user unit `packaging/maksut-self-improve.service`):** polls `self-improve/inbox`, moves valid proposals to `outbox/<id>.json` with `status: "proposed"`, and when the owner approves a request (`approvals/<id>.json`), opens a GitHub issue (`SELF_IMPROVE_GITHUB_REPO`, default `labi-le/maxwell`) via `gh` and writes `status: "issue_created"` with `issue_url`. Further discussion, implementation, and PR review happen on GitHub (via `roboomp` / `buer`).

## Single-gate lifecycle: `proposed` -> `issue_created`

1. **Proposal (`inbox/` -> `outbox/` with `status: "proposed"`):**
   - Created via `/improve <goal>` (owner in private chat) or the `propose_feature` function tool (stage `"plan"`).
   - `propose_feature` with `refines=<id>` queues a revision of an existing `proposed`, `failed`, or `rejected` request, passing `feedback` (`goal`) without marking the previous request rejected on disk while hiding superseded `proposed` parents from active queue views (`superseded_parent_ids`).
   - The orchestrator validates the request and writes `status: "proposed"` to `outbox/<id>.json`.
   - The bot notifies the owner with the proposer, goal, and two inline buttons (`В issue` / `В помойку`).
2. **Owner decision (`approvals/` or `rejections/`) and GitHub issue creation (`issue_created`):**
   - Decisions are recorded only from an owner inline button press (`DecisionGate::Issue`) or an explicit owner command in private chat (`одобри #id`, `отклони #id`, or `/improve approve|reject <id>`). Group messages and forwarded messages never approve or reject.
   - On approve, the bot asks the model for a short issue title (≤70 chars) and writes it into `approvals/<id>.json` as `title`. The host orchestrator stays agent-free: it only reads that field.
   - When approved, the orchestrator runs `gh issue create` with the summarized title (falling back to the leading clause or first 120 characters of the goal) and a body built from `предложение от @<username>` (numeric `proposed_by` when the proposer has no username), `время` from `created_at` in UTC, the full goal, and any refinement feedback. It then writes `status: "issue_created"` and `issue_url` to `outbox/<id>.json`.
   - The bot notifies the chat with the created GitHub issue URL so users can continue discussion in the issue.
   - Rejecting a proposal removes the marker and marks the task `rejected` without creating an issue.

## Task IDs, priority, and cancellation (`src/improve_commands.rs`)

- **Task IDs:** new requests get `propose-<username>` (`propose-<user_id>` when the proposer has no username). The id is also the queue file name, so a repeat proposal from the same author gets `-2`, `-3`, and so on; the first one keeps the bare `propose-<username>` form. Legacy `plan-<unix>-<chat_id>-<message_id>` tasks stay valid and still display as their trailing message id (`short_task_id`). Owner commands accept the full id or any unique fragment of it; ambiguous fragments return the candidate list instead of guessing or claiming the task is missing.
- **Batch cancel:** `отмени #id1 #id2` (or `/improve cancel <id>`) cancels pending tasks (`inbox` or waiting at `proposed`) in a single reply; already-finished tasks are reported separately.
- **Priority:** `подними #id`, `опусти #id`, `приоритет #id <n>` (clamped to `-1000..=1000`, `/improve priority <id> [n]`). Both `inbox` and `outbox` sort by `(-priority, filename)`.
- **Kill switch:** `/improve off` writes `self-improve/off`, pausing both the orchestrator loop and `tools/auto_deploy.py` until the file is removed.

## Running the orchestrator and GHCR auto-deploy on the host

After the first `.github/workflows/publish-image.yml` run creates the `ghcr.io/labi-le/maxwell` package, either set its package visibility to **Public** in GitHub Package Settings (recommended — the image contains only compiled artifacts from the public repository) or authenticate Docker on the host once with `gh auth token | docker login ghcr.io -u labi-le --password-stdin`.

```sh
mkdir -p .secrets
openssl rand -hex 32 > .secrets/deploy-hook.secret
chmod 600 .secrets/deploy-hook.secret
cp packaging/maksut-self-improve.service packaging/maksut-deploy-hook.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now maksut-self-improve.service maksut-deploy-hook.service
journalctl --user -u maksut-self-improve.service -u maksut-deploy-hook.service -f
```

The deploy itself is event-driven ([ADR 024](../adr/024-event-driven-deploy.md)): GitHub posts a `workflow_run` completion of `publish-image` to `https://hook.labile.cc/webhook/maksut-deploy`, where `tools/deploy_hook.py` checks the GitHub headers and the HMAC signature and runs `tools/published_deploy.py`; the vhost behind it allowlists GitHub's hook ranges and answers 404 on every other path. Register the repository webhook once (Settings → Webhooks, content type `application/json`, event *Workflow runs*, secret equal to `.secrets/deploy-hook.secret`). The webhook is the only trigger: no timer polls `origin/main`, so a delivery the host missed is recovered through GitHub → Webhooks → Recent Deliveries → *Redeliver*, or by `make deploy-now`, which runs the same `tools/published_deploy.py` shim. A merge that changes `tools/deploy_hook.py` needs `systemctl --user restart maksut-deploy-hook` to take effect. Both entry points run the script as published on `origin/main` (`git show origin/main:tools/auto_deploy.py`), so deploy logic follows the repository even when the checkout has uncommitted work ([ADR 023](../adr/023-auto-deploy-from-remote.md)).

A tick whose `self-improve/deployed_sha` already equals `origin/main` is not trusted blindly: it inspects the `maksut` container and runs `docker compose up -d --no-build` for it when the container is absent or stopped, or when its `Config.Image` or the `/var/lib/maksut/code/src` mount no longer match that sha, logging `auto-deploy: reconciling <sha>: <reason>`. A manual `docker compose up -d` with another `MAKSUT_IMAGE_TAG` or `MAKSUT_CODE_DIR` (or an image rollback) leaves the marker lying, and without this check the tick, `make deploy-now` and `make deploy-status` would keep reporting a deploy that the container is not running. The marker itself is only written when the sha changes. `make deploy-status` prints that same check as its `drift:` line.

The orchestrator is a long-lived Python process, so it holds the code it loaded at startup. At the top of every poll cycle it hashes its own file and re-executes itself when the checkout moved under it (`newer_source`), so a deploy takes effect without a manual `systemctl --user restart maksut-self-improve`. A file caught mid-write or unreadable keeps the running process: the new source must compile before it replaces the old one. While a task runs (an `omp` consult can take hours), the loop is inside it and keeps the old code until it returns.
