# 2026-11-02 — независимое ревью PR отдельным аккаунтом

## Context

Every pull request against this repository is opened by the `farm/*` bot account
(`int8-ptr`). `buer` already reviews pull requests on `gemini-4-argon`, but its
router drops any PR whose author is its own account
(`_pr_review_pr` → `skip: bot-authored PR` in upstream
`python/robomp/src/github_events.py`, recorded as a known gap in `docs/buer.md`).
GitHub likewise forbids a PR author from approving his own change. The result was
that no human or bot review stood between an automated branch and `main`, while
`docs/guides/workflow.md` requires an implement → review → fix loop for every
change.

A second GitHub identity was created for exactly this role. It must not be able
to push code, only to read a pull request and publish a review.

## Decision

- The reviewer is a separate account added to `labi-le/maxwell` as a **Write**
  collaborator with a **classic** `repo`-scoped PAT. Fine-grained tokens owned by
  a personal account cannot be scoped to repositories that account does not own.
- The review runs in GitHub Actions (`.github/workflows/pr-review.yml`), not in
  `buer` and not in a second roboomp stack. The checkout is the repository's
  **default branch**: pull-request head code never runs with the PAT in scope,
  which is the same reason `buer` keeps its PAT inside a separate proxy
  container.
- `tools/pr_review.py` (Python standard library only) reads the PR through the
  REST API, caps the diff at 120 000 characters, and asks the LiteLLM gateway for
  one strict-JSON review that carries `severity`, `file`, `line`, `claim`, and
  `evidence` per finding. `AGENTS.md` from that checkout is part of the
  prompt; the pull request text is declared untrusted data.
- The published event is `REQUEST_CHANGES` when a P0/P1 finding exists or the
  model's verdict asks for changes, otherwise `APPROVE`. Reviews are body-only:
  inline comments are not posted, and the repository's branch protection, not a
  review body, is what makes them binding.
- One review per head SHA; drafts, self-authored PRs, empty diffs, and PRs whose
  secrets are unavailable (forks) are skipped. A malformed model answer is retried
  once and then fails the job, so a broken reviewer never silently approves.
- Branch protection on `main` requires one approving review from a collaborator.

## Alternatives rejected

- **Second roboomp instance.** It would reuse the inline-comment rubric and the
  existing prompt, but costs a second webhook, an nginx vhost, a new agenix
  secret, and two more containers on a host that already runs the triage stack.
  The selected workflow needs no runtime infrastructure and cannot interfere with
  `buer`'s publishing gates.
- **Patching `buer` to review its own PRs with a second token.** The route and
  the gh-proxy would both need local patches, and one process would hold two
  identities — the failure mode this design exists to prevent.

## Consequences

- Automated PRs now get a critique and cannot be merged while it asks for
  changes; the reviewer re-reviews every push because the workflow runs on
  `synchronize`.
- The reviewer must be installed before branch protection is enabled, otherwise
  every merge blocks: the `REVIEWER_TOKEN` and `REVIEWER_LLM_API_KEY` secrets and
  the collaborator invitation are prerequisites, documented in
  `docs/guides/review-bot.md`.
- Fork pull requests receive no secrets and stay unreviewed by this path; they
  still need a human review.
- Each push costs one model call on `gemini-4-argon` and one Actions job; the
  repository is public, so the minutes are free.
