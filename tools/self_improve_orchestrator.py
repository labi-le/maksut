#!/usr/bin/env python3
"""Host-side, agent-free orchestrator for Maksut feature requests.

The chat bot writes feature requests to `self-improve/inbox/`. This process
moves valid requests to `outbox/` with `status: "proposed"` so the bot can show
the owner inline approval buttons (`В issue` / `В помойку`), and when the owner
approves a request (`self-improve/approvals/<id>.json`), opens a GitHub issue via
`gh` so further discussion and implementation happen on GitHub (via roboomp).
It never interprets chat text as a shell command.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(os.environ.get("SELF_IMPROVE_REPO", "/home/labile/projects/maxwell"))
QUEUE = Path(os.environ.get("SELF_IMPROVE_DIR", str(REPO / "self-improve")))
OWNER = int(os.environ.get("OWNER_ID", "0"))
GITHUB_REPO = os.environ.get("SELF_IMPROVE_GITHUB_REPO", "labi-le/maxwell")
GH = os.environ.get("SELF_IMPROVE_GH", shutil.which("gh") or "gh")
POLL_SECONDS = 5
MAX_PROCESSED = 1000


def log(message: str) -> None:
    print(f"self-improve: {message}", file=sys.stderr, flush=True)


def run(
    args: list[str],
    timeout: int = 120,
    env: dict | None = None,
    check: bool = False,
    input: str | None = None,
) -> subprocess.CompletedProcess:
    return subprocess.run(
        args,
        timeout=timeout,
        env=env,
        text=True,
        capture_output=True,
        check=check,
        input=input,
    )


def atomic_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(f".tmp-{os.getpid()}")
    tmp.write_text(json.dumps(value, ensure_ascii=False))
    tmp.chmod(0o600)
    os.replace(tmp, path)


def status(task: dict, state: str, summary: str = "", **extra) -> None:
    def pick(name: str, default=""):
        return extra[name] if name in extra else task.get(name, default)

    value = {
        "id": task["id"],
        "status": state,
        "summary": summary[:2000],
        "chat_id": int(pick("chat_id", 0) or 0),
        "goal": pick("goal"),
        "stage": pick("stage"),
        "source_chat_id": int(pick("source_chat_id", 0) or 0),
        "proposed_by": int(pick("proposed_by", 0) or 0),
        "proposed_by_name": pick("proposed_by_name"),
        "issue_url": pick("issue_url"),
        "priority": int(pick("priority", 0) or 0),
        "created_at": int(task.get("created_at") or extra.get("created_at") or time.time()),
        "updated_at": int(time.time()),
    }
    for name in ("requested_by", "parent", "feedback"):
        found = extra[name] if name in extra else task.get(name)
        if found is not None:
            value[name] = found
    atomic_json(QUEUE / "outbox" / f"{task['id']}.json", value)
    log(f"task {task['id']}: {state}")


def create_issue(task: dict) -> str:
    body_path = QUEUE / "tmp" / f"{task['id']}-issue.md"
    body_path.parent.mkdir(parents=True, exist_ok=True)
    proposer = str(task.get("proposed_by_name") or "").strip()
    proposed_by = int(task.get("proposed_by", 0) or 0)
    lines = [
        f"<!-- maksut-self-improve: {task['id']} -->",
        f"Цель: {task.get('goal', '')}",
    ]
    if proposer and proposed_by != 0 and proposed_by != OWNER:
        lines.append(f"Предложил: {proposer} ({proposed_by})")
    feedback = str(task.get("feedback") or "").strip()
    if feedback and feedback != str(task.get("goal") or "").strip():
        lines.append(f"\nУточнение: {feedback}")
    body_path.write_text("\n".join(lines) + "\n")
    body_path.chmod(0o600)
    title = "Maksut: " + " ".join(str(task.get("goal", "")).split())[:200]
    try:
        proc = run(
            [
                GH,
                "issue",
                "create",
                "--repo",
                GITHUB_REPO,
                "--title",
                title,
                "--body-file",
                str(body_path),
            ],
            timeout=60,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        log(f"task {task['id']}: issue creation error: {type(error).__name__}")
        return ""
    finally:
        body_path.unlink(missing_ok=True)
    if proc.returncode != 0:
        log(f"task {task['id']}: issue creation failed")
        return ""
    url = (proc.stdout or "").strip().splitlines()[-1] if (proc.stdout or "").strip() else ""
    return url if url.startswith("https://github.com/") else ""


def propose(task: dict) -> None:
    goal = str(task.get("goal") or "")
    status(
        task,
        "proposed",
        goal[:2000],
        stage="plan",
        chat_id=task.get("chat_id", 0),
    )


def valid_approval_marker(approval: dict, task_id: str) -> bool:
    return (
        isinstance(approval, dict)
        and approval.get("id") == task_id
        and approval.get("requested_by") == OWNER
    )


def queue_sort_key(path: Path) -> tuple[int, str]:
    try:
        value = json.loads(path.read_text())
        priority = int(value.get("priority", 0))
    except (OSError, ValueError, TypeError, json.JSONDecodeError):
        priority = 0
    return (-priority, path.name)


def process_approvals() -> None:
    for path in sorted((QUEUE / "outbox").glob("*.json"), key=queue_sort_key):
        try:
            value = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        state = value.get("status")
        task_id = value.get("id")
        if not isinstance(task_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", task_id):
            continue
        if state != "proposed":
            continue
        rejection = QUEUE / "rejections" / f"{task_id}.json"
        approval = QUEUE / "approvals" / f"{task_id}.json"
        if rejection.exists():
            rejection.unlink(missing_ok=True)
            status(value, "rejected", "owner rejected")
            continue
        if not approval.exists():
            continue
        try:
            approved = json.loads(approval.read_text())
        except (OSError, json.JSONDecodeError):
            approved = {}
        if not valid_approval_marker(approved, task_id):
            approval.unlink(missing_ok=True)
            log(f"task {task_id}: ignored invalid approval marker")
            continue
        approval.unlink(missing_ok=True)
        try:
            issue_url = create_issue(value)
        except Exception as error:
            log(f"task {task_id}: {type(error).__name__}")
            issue_url = ""
        if not issue_url:
            status(value, "failed", "не удалось создать issue на GitHub")
            continue
        status(value, "issue_created", "issue создана", issue_url=issue_url)


def valid_task(value: dict) -> bool:
    return (
        isinstance(value, dict)
        and isinstance(value.get("id"), str)
        and re.fullmatch(r"[A-Za-z0-9_-]{1,80}", value["id"]) is not None
        and value.get("requested_by") == OWNER
        and isinstance(value.get("goal"), str)
        and 0 < len(value["goal"]) <= 2000
        and value.get("stage", "") in ("", "plan")
        and isinstance(value.get("chat_id", 0), int)
    )


def main() -> int:
    if OWNER == 0:
        log("OWNER_ID is missing")
        return 1
    log("orchestrator started")
    QUEUE.mkdir(parents=True, exist_ok=True)
    for name in ("inbox", "outbox", "approvals", "rejections", "tmp", "notified"):
        (QUEUE / name).mkdir(parents=True, exist_ok=True)
    processed: dict[str, None] = {}
    while True:
        if (QUEUE / "off").exists():
            time.sleep(POLL_SECONDS)
            continue
        process_approvals()
        for path in sorted((QUEUE / "inbox").glob("*.json"), key=queue_sort_key):
            process_approvals()
            try:
                task = json.loads(path.read_text())
            except (OSError, json.JSONDecodeError):
                path.unlink(missing_ok=True)
                continue
            if not valid_task(task) or task["id"] in processed:
                path.unlink(missing_ok=True)
                continue
            processed[task["id"]] = None
            if len(processed) > MAX_PROCESSED:
                del processed[next(iter(processed))]
            path.unlink(missing_ok=True)
            try:
                propose(task)
            except Exception as error:
                log(f"task {task.get('id', '?')}: {type(error).__name__}")
                status(task, "failed", f"orchestrator error: {type(error).__name__}")
        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    sys.exit(main())
