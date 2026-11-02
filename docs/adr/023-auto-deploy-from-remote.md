# 2026-11-02 — Auto-deploy follows `origin/main`, not the host checkout

## Context

[ADR 022](022-ghcr-auto-deploy.md) made `tools/auto_deploy.py` skip every run
unless the checkout sat on `main` with a clean working tree, and fast-forward it
before recreating the container. That coupling turned uncommitted host work into
a deployment freeze: on 2026-11-02 eight tracked files of in-progress work kept
the timer logging `skip: working tree has uncommitted tracked changes` for five
hours while merges kept landing. Images were published, the container kept
running an older build, and the bot answered from it. The same checkout also
feeds `/var/lib/maksut/code`, so a dirty tree made `inspect_code` read
uncommitted work instead of the commit the container was built from.

## Decision

- A deploy is derived from `origin/main` alone. `git fetch` resolves the target
  SHA; `docker pull ghcr.io/labi-le/maxwell:<sha>` fetches the image;
  `compose.yaml` comes from `git show <sha>:compose.yaml`, written to
  `.auto-deploy.compose.yaml` beside the checkout so relative bind mounts and
  `env_file` resolve exactly as before and removed afterwards.
- The sources for `inspect_code` come from that commit too: `git archive <sha>`
  over `src`, `tools`, `docs`, `Cargo.toml`, `README.md` and `AGENTS.md` is
  extracted into `self-improve/code/<sha>`, and `compose.yaml` mounts it through
  `${MAKSUT_CODE_DIR:-.}`, so a manual `docker compose up` still reads the
  checkout.
- The checkout is fast-forwarded only when it is on `main` with no tracked
  changes. Otherwise the run logs `checkout left as is` and deploys anyway.
  Local edits are never reset, stashed, or checked out over; the checkout simply
  catches up on a later run.
- Both entry points (`tools/published_deploy.py` for the timer and the
  webhook) execute the published script rather than the working copy:
  `git show origin/main:tools/auto_deploy.py` into a temporary file, then
  `python3` on that copy, after checking `self-improve/off`. New deploy logic
  therefore takes effect without the checkout advancing, and a stale checkout
  can never pin old deploy logic. [ADR 024](024-event-driven-deploy.md) changes
  only the trigger; this script stays the single deploy implementation.
- `docker pull` reporting `manifest unknown` (or `not found`) means the image is
  still building: the marker is not advanced, the container is untouched, and
  the next tick retries.
- After a successful deploy only the current export and image tag are kept;
  `self-improve/code/<previous>` and the previous `<sha>` tag are pruned.

## Consequences

- Merges reach the container while the checkout is dirty, on a feature branch,
  or behind `origin/main`.
- `inspect_code` always reads the deployed commit, never the developer's working
  tree, so the bot cannot cite code that only exists locally.
- A dirty checkout logs `checkout left as is: uncommitted tracked changes` when
  a deploy or a fallback poll runs, until it is committed; nothing else about
  the deploy depends on it.
- The kill switch (`self-improve/off`) still pauses the timer before any command
  runs.
