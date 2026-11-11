# Independent pull-request reviewer

> **When to read this:** Read before touching `.github/workflows/pr-review.yml`, `tools/pr_review.py`, `tools/test_pr_review.py`, or the `REVIEWER_*` repository secrets and the reviewer account behind them.

Every pull request in this repository is opened by the `farm/*` bot account (`int8-ptr`). `buer` — the GitHub triage bot — refuses to review a pull request whose author is its own account: `_pr_review_pr` in upstream `python/robomp/src/github_events.py` returns `skip: bot-authored PR`, and `docs/buer.md` records that as a known gap. The critique therefore has to come from an account that is not the author: either a dedicated reviewer account or the repository owner's own account. Both work; the workflow only ever knows the identity behind `REVIEWER_TOKEN`, and the review body always states that the review is automated.

## What runs

`.github/workflows/pr-review.yml` triggers on `opened`, `synchronize`, `reopened`, and `ready_for_review`, and can be started by hand with `workflow_dispatch` plus a `pr` input. It checks out the repository's **default branch**, so the code that holds `REVIEWER_TOKEN` in its environment is always the reviewer as merged, never the pull request itself.

`tools/pr_review.py` then:

1. Reads the PR (`GET /repos/{repo}/pulls/{n}`) and its diff (`Accept: application/vnd.github.v3.diff`), capped at 120 000 characters.
2. Skips silently when the token is empty (fork pull requests never receive repository secrets), when the PR is a draft, when the author is `dependabot[bot]` (handled by `.github/workflows/automerge-dependabot.yml`), when the reviewer is the author (GitHub forbids approving your own PR), or when the reviewer already reviewed this exact head SHA — a re-run never duplicates a review, and a push produces a new one.
3. Asks the gateway for one strict-JSON review with `severity`/`file`/`line`/`claim`/`evidence` per finding. The prompt carries `AGENTS.md` plus every `docs/guides/*.md` whose trigger matches a path in the diff (mapped from the `AGENTS.md` guide table), and exposes a read-only `read_file` tool (up to 6 calls per review, 16 000 characters per file, restricted to non-secret repository files in the base checkout) so the model can inspect `docs/adr/*`, other guides, or callers in `src/` and `tools/` on demand; the PR text is declared untrusted data.
4. Publishes `REQUEST_CHANGES` when any P0/P1 finding exists or the model's verdict says so, otherwise `APPROVE`, with the summary and findings rendered as a Markdown body. An approval with no findings carries an empty body. This is a body-only review: inline comments are not posted.

A model answer that fails schema validation is retried once with the rejection reason appended; a second failure fails the job instead of approving anything. GitHub, gateway, and parsing failures exit non-zero for the same reason.

## Setup

The reviewer is whichever account owns `REVIEWER_TOKEN`. Two workable choices:

- **Owner's own account (recommended).** Create a *fine-grained* PAT scoped to `labi-le/maksut` with the single writable permission **Pull requests: Read and write**, plus the mandatory **Metadata: Read**; set an expiry and leave everything else, including **Contents**, at **No access**. A personal account owns the repository, so the fine-grained token can select it — the limitation described below only applies to accounts that do not own the repository. No collaborator invitation is needed, and the token cannot touch any other repository. Reviews appear under the owner's login and carry only the summary and findings; an approval with no findings has an empty body, so the login is the only authorship signal. Do not merge on an approval without reading it.
- **Dedicated reviewer account.** Invite it as a collaborator with **Write** (an outside account cannot submit an approving or requesting-changes review without it; only collaborators' reviews count for branch protection) and create a **classic** PAT with the `repo` scope. A fine-grained token owned by a personal account cannot be scoped to repositories that account does not own, the same limitation `docs/buer.md` records for the triage bot; keep that account invited to this repository only, so the classic scope reaches nothing else.

Classic scopes cannot express "review only": the smallest scope that can publish a review is `public_repo`, which also grants read/write access to code, collaborators, commit statuses, and deployments in every public repository the account can write to. A classic token therefore does not meet the read-only requirement this workflow assumes; use it only on an account invited to this repository alone, and prefer the fine-grained token above.

Then store two repository secrets:

```sh
gh secret set REVIEWER_TOKEN --repo labi-le/maksut       # PAT of the chosen reviewer identity
gh secret set REVIEWER_LLM_API_KEY --repo labi-le/maksut # LiteLLM key with gemini-4-argon access
```

The bot's own `LLM_API_KEY` is restricted to `deepseek-v4.1-flash` and answers `403 The requested model 'gemini-4-argon' is not available for this API key`; the reviewer needs a key that lists that model. Switching identities later only means replacing `REVIEWER_TOKEN`; reviews already published stay attributed to the account that posted them.

Optionally pin the model per repository without editing the workflow: `gh variable set PR_REVIEW_MODEL --repo labi-le/maksut --body gemini-4-argon`. `LLM_BASE_URL` defaults to `https://llm.labile.cc/v1`.

## Token scope and trust boundary

The reviewer token grants exactly what the script needs and nothing else. Keep the permission list this short:

| Permission | Level | Why |
|---|---|---|
| Pull requests | Read and write | read the pull request and its diff, list earlier reviews, publish one review |
| Metadata | Read | mandatory companion of every fine-grained token |
| everything else | No access | no call in `tools/pr_review.py` touches it; the checkout uses `GITHUB_TOKEN`, not this PAT |

**Pull requests: write is the minimum GitHub offers** for publishing a review; there is no comment-only scope. It lets the token edit a pull request's title or body and dismiss reviews. It does not let it push commits, create branches, merge, edit workflow files, write secrets, or change repository settings — all of those require `Contents: write` or a wider permission, which stays off. With the token stolen, the worst case is unwanted reviews and pull-request metadata in this one public repository; merging still takes a human click.

What keeps the token away from pull-request code:

- the workflow runs on `pull_request` for same-repository branches; fork pull requests never receive repository secrets, and the job exits before touching the network;
- it checks out the **default branch**, never the pull request head, with `persist-credentials: false`;
- its own `GITHUB_TOKEN` is limited to `contents: read` and is not used by the script;
- the pull request text is untrusted input to the model only. `read_file` resolves paths inside the base checkout, rejects symlinks, hidden paths (except `.github` and `.dockerignore`), and secret/state directories (`.git`, `.env`, `.secrets`, `cookies`, `self-improve`, `self_improve`, `target`), and nothing from the pull request is executed.

## Blocking merges

The review event alone does not stop a merge; GitHub needs branch protection to act on it. This payload requires one approving review, dismisses stale approvals, and also requires the `review` job itself, so a reviewer that crashes blocks the merge instead of letting it through:

```sh
cat >/tmp/maksut-protection.json <<'JSON'
{
  "required_status_checks": {"strict": true, "contexts": ["review"]},
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true
  },
  "restrictions": null
}
JSON
gh api -X PUT repos/labi-le/maksut/branches/main/protection --input /tmp/maksut-protection.json
```

Run it only after `REVIEWER_TOKEN` works: until the reviewer can submit an `APPROVE`, every pull request is blocked. The required `review` check also blocks fork pull requests, which never receive the secrets; keep the requirement off if forks should merge without the bot.

With `enforce_admins: false` an administrator can still merge without the approval. If the reviewer is the owner's own account, that makes the gate advisory for the owner; set `enforce_admins: true` to make the bot's `APPROVE` the thing that unblocks every merge, including the owner's.

`REQUEST_CHANGES` blocks the merge while it stands; a clean `APPROVE` satisfies the one-approval rule, and the workflow re-reviews every push so a dismissed stale approval is replaced automatically.

## Failure modes

| Symptom | Cause |
|---|---|
| `skip #N: GH_TOKEN is empty` | fork PR, or `REVIEWER_TOKEN` missing/not yet set |
| `skip #N: reviewer is the pull request author` | the PR was opened by the reviewer account itself |
| `HTTP 403` on the gateway | the key may not use `gemini-4-argon` |
| `HTTP 403`/`422` on `POST .../reviews` | reviewer is not a collaborator, the PAT lost `repo`, or a fine-grained PAT is read-only; the error prints `(accepted: pull_requests=write)` in that case |
| `model answer contains no JSON object` | the model ignored the schema twice; the job fails and the PR stays unreviewed |
| no review on a push | the workflow only runs for the four event types above and once per head SHA |

## Limits and costs

- Diff content is capped at 120 000 characters, the PR body at 6 000, `AGENTS.md` at 24 000, each preloaded guide or `read_file` response at 16 000 (up to 6 `read_file` calls per review), and the published body at 60 000; anything larger is truncated with a visible marker so the finding set is honest about its own coverage.
- One review per head SHA per repository: one gateway conversation per push (typically a single call when preloaded guides suffice, plus up to 6 `read_file` tool rounds when the model inspects extra files), plus one retry when the final answer is malformed.
- The workflow inherits the repository's Actions minutes; `labi-le/maksut` is public, so they are free.
