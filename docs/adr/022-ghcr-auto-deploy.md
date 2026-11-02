# 2026-11-02 — GHCR image publish and host auto-deploy on push to `main`

## Context

Merging a pull request into `main` previously left deployment manual: someone had
to `git pull` on the host and run `docker compose -f compose.yaml build maksut`
followed by `docker compose -f compose.yaml up -d maksut`. Building the Rust
binary inside Docker on the host takes ~70 seconds of CPU/RAM and diverges from
the repository rule that the server never compiles Rust locally.

Meanwhile, the repository is public (`labi-le/maxwell`), `.dockerignore` excludes
all runtime state (`.env`, `.secrets/`, `cookies.txt`, `*.db`, `self-improve/`),
the builder stage compiles `maksut` from the `.dockerignore`-filtered checkout,
and the final runtime stage copies only `/usr/local/bin/maksut`,
`tools/twitter_images.py`, `sticker_sets.txt`, and `sticker_descriptions.json`.
All secrets and state are mounted at runtime by `compose.yaml`.

## Decision

- Add `.github/workflows/publish-image.yml`: on every push to `main` (and
  `workflow_dispatch`), GitHub Actions builds `Dockerfile` with BuildKit GHA
  layer caching and pushes `ghcr.io/labi-le/maxwell:main` and
  `ghcr.io/labi-le/maxwell:<git-sha>` using `GITHUB_TOKEN` (`packages: write`).
- Set `image: ghcr.io/labi-le/maxwell:${MAKSUT_IMAGE_TAG:-main}` on the `maksut`
  service in `compose.yaml` while keeping `build: .` for manual local container
  verification when needed. Either set the GHCR package visibility to **Public**
  (after its first push) or run `docker login ghcr.io` on the host so `docker pull`
  is authorized.
- Add `tools/auto_deploy.py` (`tools/test_auto_deploy.py`) and systemd user units
  `packaging/maksut-auto-deploy.service` + `packaging/maksut-auto-deploy.timer`
  (polling every 45 seconds):
  1. Acquires a non-blocking flock (`self-improve/auto-deploy.lock`) and exits
     immediately if `self-improve/off` exists, the checkout is not on `main`, or
     tracked files are dirty.
  2. Runs `git fetch origin main` and compares `origin/main` SHA against
     `self-improve/deployed_sha` and `HEAD`.
  3. When a new SHA is present, runs `docker pull ghcr.io/labi-le/maxwell:<sha>`
     first (logging the last stderr line on failure so auth errors vs. pending
     builds are visible in the journal). If the image is not available yet,
     exits without advancing the checkout so `/var/lib/maksut/code` stays
     aligned with the running container.
  4. Fast-forwards the checkout (`git merge --ff-only origin/main`), runs
     `MAKSUT_IMAGE_TAG=<sha> docker compose -f compose.yaml up -d --no-build maksut`,
     atomically records `<sha>` in `self-improve/deployed_sha`, and removes the
     superseded `ghcr.io/labi-le/maxwell:<previous_sha>` tag so host disk does
     not grow per merge.

## Consequences

- Pushing or squash-merging to `main` automatically updates both the host checkout
  (for `inspect_code` bind mounts) and the running `maksut` container as soon as
  the GHCR image finishes building in GitHub Actions.
- Zero secrets or SSH keys are exposed to GitHub Actions, and the server no
  longer compiles Rust during routine deployments.
- Touching `self-improve/off` (`/improve off`) pauses both the self-improve
  orchestrator and auto-deploy, and dirty local edits on the host checkout are
  never overwritten.
