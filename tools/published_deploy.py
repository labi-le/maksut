#!/usr/bin/env python3
"""Run `tools/auto_deploy.py` as published on the remote branch.

The checkout is only a bootstrap: deploy logic follows the repository even when
the working tree carries uncommitted work or sits behind the remote. Both the
timer and the webhook call this shim instead of the working copy.
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT = "tools/auto_deploy.py"
GIT_TIMEOUT = 60


def log(message: str) -> None:
    print(f"published-deploy: {message}", file=sys.stderr, flush=True)


def is_paused(state_dir: Path) -> bool:
    return (state_dir / "off").exists()


def published_source(repo: Path, branch: str) -> bytes | None:
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "show", f"origin/{branch}:{SCRIPT}"],
            timeout=GIT_TIMEOUT,
            capture_output=True,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        log(f"git show failed: {type(error).__name__}")
        return None
    if proc.returncode != 0 or not proc.stdout:
        detail = (proc.stderr or b"").decode(errors="replace").strip().splitlines()
        log(f"git show failed: {detail[-1][:200] if detail else 'no output'}")
        return None
    return proc.stdout


def main() -> int:
    repo = Path(os.environ.get("AUTO_DEPLOY_REPO", "/home/labile/projects/maksut"))
    branch = os.environ.get("AUTO_DEPLOY_BRANCH", "main")
    state_dir = Path(os.environ.get("AUTO_DEPLOY_STATE_DIR", str(repo / "self-improve")))
    if is_paused(state_dir):
        return 0
    source = published_source(repo, branch)
    if source is None:
        return 1
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    with tempfile.NamedTemporaryFile(
        "wb", prefix="maksut-auto-deploy-", suffix=".py", dir=runtime_dir, delete=False
    ) as handle:
        handle.write(source)
        script = Path(handle.name)
    try:
        return subprocess.run([sys.executable, str(script)], timeout=None, check=False).returncode
    finally:
        script.unlink(missing_ok=True)


if __name__ == "__main__":
    sys.exit(main())
