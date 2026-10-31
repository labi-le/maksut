# Feature request intake and GitHub issue orchestrator

> **When to read this:** Read before touching `src/self_improve.rs`, `src/improve_store.rs`, `src/improve_views.rs`, `src/improve_commands.rs`, `src/code_inspect.rs`, `tools/self_improve_orchestrator.py`, or `packaging/maksut-self-improve.service`.

## Architecture: Telegram feature intake -> GitHub issue (`roboomp`)

The self-improvement intake is split across two isolated processes so the Telegram bot never executes shell or `gh` commands directly:

1. **Bot (`src/self_improve.rs`, inside the `maksut` container):** writes feature requests (wishlist items) to `self-improve/inbox/*.json`, reads status from `self-improve/outbox/*.json`, sends notifications (`improvement_notifier` polling every 5s and tracking `notified/`), and writes owner decisions to `approvals/` and `rejections/`.
2. **Host orchestrator (`tools/self_improve_orchestrator.py`, systemd user unit `packaging/maksut-self-improve.service`):** polls `self-improve/inbox`, moves valid proposals to `outbox/<id>.json` with `status: "proposed"`, and when the owner approves a request (`approvals/<id>.json`), opens a GitHub issue (`SELF_IMPROVE_GITHUB_REPO`, default `labi-le/maxwell`) via `gh` and writes `status: "issue_created"` with `issue_url`. Further discussion, implementation, and PR review happen on GitHub (via `roboomp` / `buer`).

## Single-gate lifecycle: `proposed` -> `issue_created`

1. **Proposal (`inbox/` -> `outbox/` with `status: "proposed"`):**
   - Created via `/improve <goal>` (owner in private chat) or the `propose_feature` function tool (stage `"plan"`).
   - `propose_feature` with `refines=<id>` queues a revision of an existing `proposed` or `failed` request, passing `feedback` (`goal`) without marking the previous request rejected.
   - The orchestrator validates the request and writes `status: "proposed"` to `outbox/<id>.json`.
   - The bot notifies the owner with the proposer, goal, and two inline buttons (`В issue` / `В помойку`).
2. **Owner decision (`approvals/` or `rejections/`) and GitHub issue creation (`issue_created`):**
   - Decisions are recorded only from an owner inline button press (`DecisionGate::Issue`) or an explicit owner command in private chat (`одобри #id`, `отклони #id`, or `/improve approve|reject <id>`). Group messages and forwarded messages never approve or reject.
   - When approved, the orchestrator runs `gh issue create` with the goal, proposer attribution, and any refinement feedback, then writes `status: "issue_created"` and `issue_url` to `outbox/<id>.json`.
   - The bot notifies the chat with the created GitHub issue URL so users can continue discussion in the issue.
   - Rejecting a proposal removes the marker and marks the task `rejected` without creating an issue.

## Task IDs, priority, and cancellation (`src/improve_commands.rs`)

- **Task ID matching:** IDs such as `plan-1793270374-1002045081420-214537` display as `#214537` (`short_task_id`). Owner commands accept the full ID, the short numeric tail (`214537`), or any unique numeric prefix/suffix (`37`). Ambiguous fragments return the candidate list instead of guessing or claiming the task is missing.
- **Batch cancel:** `отмени #id1 #id2` (or `/improve cancel <id>`) cancels pending tasks (`inbox` or waiting at `proposed`) in a single reply; already-finished tasks are reported separately.
- **Priority:** `подними #id`, `опусти #id`, `приоритет #id <n>` (clamped to `-1000..=1000`, `/improve priority <id> [n]`). Both `inbox` and `outbox` sort by `(-priority, filename)`.
- **Kill switch:** `/improve off` writes `self-improve/off`, pausing the orchestrator loop until the file is removed.
## Running the orchestrator on the host

```sh
cp packaging/maksut-self-improve.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now maksut-self-improve.service
journalctl --user -u maksut-self-improve.service -f
```
