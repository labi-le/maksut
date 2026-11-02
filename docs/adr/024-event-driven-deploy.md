# 2026-11-02 — Deploys are triggered by a GitHub webhook, not by polling

## Context

[ADR 022](022-ghcr-auto-deploy.md) and [ADR 023](023-auto-deploy-from-remote.md)
polled `origin/main` every 45 seconds: a `git fetch`, a `rev-parse`, and a
worktree scan — roughly 1,920 rounds a day to learn about an event GitHub
already knows about. The polling was cheap but arbitrary: it added up to 45
seconds of delay after the image finished building, it filled the journal with
`checkout left as is` lines, and it raced the publish workflow often enough to
log `image ... is not published yet: manifest unknown`.

## Decision

- `tools/deploy_hook.py` is a stdlib HTTP listener on
  `127.0.0.1:${MAKSUT_HOOK_PORT}` (`6551`) behind the host reverse proxy at
  `${MAKSUT_HOOK_PATH}` (`/webhook/maksut-deploy`), packaged as
  `packaging/maksut-deploy-hook.service`. It accepts a POST only when all of the
  following hold: `X-Hub-Signature-256` equals the HMAC-SHA256 of the raw body
  under `MAKSUT_HOOK_SECRET_FILE` (constant-time compare), the body is a
  `workflow_run` payload with `action == "completed"`,
  `workflow_run.name == "publish-image"`, `conclusion == "success"`,
  `head_branch == "main"`, and `event` in `push` / `workflow_dispatch`.
  Everything else is refused (401 for a bad signature, 404 for another path, 405
  for GET, 413 for oversize bodies) or ignored with 204.
- `workflow_run` is chosen over `push`: the event arrives after the image is
  published, so the deploy never races the registry.
- The request is not held open for the deploy. The hook answers 202 and starts
  `tools/published_deploy.py` in a single background thread; a burst of events
  coalesces into the deploy already running.
- `tools/published_deploy.py` is the one entry point for both the hook and the
  timer: it checks `self-improve/off` first, extracts `tools/auto_deploy.py`
  from `origin/main`, and executes that copy. Deploy logic therefore follows the
  repository, never the working tree.
- The timer becomes a safety net instead of the trigger:
  `OnUnitActiveSec=30min` in `packaging/maksut-auto-deploy.timer`, covering lost
  deliveries and failed runs.

## Consequences

- Merging to `main` deploys within seconds of the publish workflow finishing,
  with no polling loop and no `manifest unknown` retries in the journal.
- The public endpoint exposes exactly one path, and the only thing it can
  trigger is a deploy of whatever `origin/main` holds; the payload can never
  name another ref, another image, or another command.
- One-time setup on the host: the reverse-proxy location, the secret file, the
  service. One-time setup on the repository: a webhook (Settings → Webhooks,
  content type `application/json`, secret matching `MAKSUT_HOOK_SECRET_FILE`,
  event: *Workflow runs*).
- If the hook is unreachable, the 30-minute timer still converges; a deploy can
  then lag up to 30 minutes instead of 45 seconds.
- A webhook secret is a shared secret between GitHub and the host: rotating it
  means updating both sides, and a mismatch shows up as 401 lines in
  `journalctl --user -u maksut-deploy-hook`.
