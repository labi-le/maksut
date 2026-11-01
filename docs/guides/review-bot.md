# Independent pull-request reviewer

> **When to read this:** Read before touching `.github/workflows/pr-review.yml`, `tools/pr_review.py`, `tools/test_pr_review.py`, or the `REVIEWER_*` repository secrets and the reviewer account behind them.

Every pull request in this repository is opened by the `farm/*` bot account (`int8-ptr`). `buer` — the GitHub triage bot — refuses to review a pull request whose author is its own account: `_pr_review_pr` in upstream `python/robomp/src/github_events.py` returns `skip: bot-authored PR`, and `docs/buer.md` records that as a known gap. A second identity therefore has to do the critique, which is what this workflow is: a separate GitHub account, a separate `repo`-scoped PAT, and `gemini-4-argon` through the LiteLLM gateway.

## What runs

`.github/workflows/pr-review.yml` triggers on `opened`, `synchronize`, `reopened`, and `ready_for_review`, and can be started by hand with `workflow_dispatch` plus a `pr` input. It checks out the **base** revision, so the code that holds `REVIEWER_TOKEN` in its environment is always the reviewed branch's ancestor, never the pull request itself.

`tools/pr_review.py` then:

1. Reads the PR (`GET /repos/{repo}/pulls/{n}`) and its diff (`Accept: application/vnd.github.v3.diff`), capped at 120 000 characters.
2. Skips silently when the token is empty (fork pull requests never receive repository secrets), when the PR is a draft, when the reviewer is the author (GitHub forbids approving your own PR), or when the reviewer already reviewed this exact head SHA — a re-run never duplicates a review, and a push produces a new one.
3. Asks the gateway for one strict-JSON review with `severity`/`file`/`line`/`claim`/`evidence` per finding. The prompt carries `AGENTS.md` from the base checkout and requires two passes, architecture and performance; the PR text is declared untrusted data.
4. Publishes `REQUEST_CHANGES` when any P0/P1 finding exists or the model's verdict says so, otherwise `APPROVE`, with the findings rendered as a Markdown body. This is a body-only review: inline comments are not posted.

A model answer that fails schema validation is retried once with the rejection reason appended; a second failure fails the job instead of approving anything. GitHub, gateway, and parsing failures exit non-zero for the same reason.

## Setup

1. Create the reviewer account, invite it to `labi-le/maxwell` as a collaborator with **Write** (an outside account cannot submit an approving or requesting-changes review without it; only collaborators' reviews count for branch protection).
2. Create a **classic** PAT on that account with the `repo` scope. A fine-grained token owned by a personal account cannot be scoped to repositories the account does not own, the same limitation `docs/buer.md` records for the triage bot.
3. Store two repository secrets:

   ```sh
   gh secret set REVIEWER_TOKEN --repo labi-le/maxwell       # reviewer PAT
   gh secret set REVIEWER_LLM_API_KEY --repo labi-le/maxwell # LiteLLM key with gemini-4-argon access
   ```

   The bot's own `LLM_API_KEY` is restricted to `deepseek-v4.1-flash` and answers `403 The requested model 'gemini-4-argon' is not available for this API key`; the reviewer needs a key that lists that model.
4. Optionally pin the model per repository without editing the workflow: `gh variable set PR_REVIEW_MODEL --repo labi-le/maxwell --body gemini-4-argon`. `LLM_BASE_URL` defaults to `https://llm.labile.cc/v1`.

## Blocking merges

The review event alone does not stop a merge; GitHub needs branch protection to act on it:

```sh
gh api -X PUT repos/labi-le/maxwell/branches/main/protection \
  -f 'required_status_checks=' \
  -F 'enforce_admins=false' \
  -F 'required_pull_request_reviews[required_approving_review_count]=1' \
  -F 'restrictions=' 
```

`REQUEST_CHANGES` blocks the merge while it stands; a clean `APPROVE` satisfies the one-approval rule. Leave `dismiss_stale_reviews` off only if the same head SHA is enough evidence — the workflow re-reviews every push, so enabling dismissal is safe and stricter.

## Failure modes

| Symptom | Cause |
|---|---|
| `skip #N: GH_TOKEN is empty` | fork PR, or `REVIEWER_TOKEN` missing/not yet set |
| `skip #N: reviewer is the pull request author` | the PR was opened by the reviewer account itself |
| `HTTP 403` on the gateway | the key may not use `gemini-4-argon` |
| `HTTP 403`/`422` on `POST .../reviews` | reviewer is not a collaborator, or the PAT lost `repo` |
| `model answer contains no JSON object` | the model ignored the schema twice; the job fails and the PR stays unreviewed |
| no review on a push | the workflow only runs for the four event types above and once per head SHA |

## Limits and costs

- Diff content is capped at 120 000 characters, the PR body at 6 000, `AGENTS.md` at 24 000, and the published body at 60 000; anything larger is truncated with a visible marker so the finding set is honest about its own coverage.
- One review per head SHA per repository: at most one gateway call per push, plus one retry when the answer is malformed.
- The workflow inherits the repository's Actions minutes; `labi-le/maxwell` is public, so they are free.
