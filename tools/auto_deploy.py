#!/usr/bin/env python3
"""Host-side auto-deploy helper for Maksut.

Fetches `origin/main`, pulls `ghcr.io/labi-le/maxwell:<sha>` once published by
`.github/workflows/publish-image.yml`, fast-forwards the checkout when it is on
`main` with a clean working tree, recreates the `maksut` container without
compiling Rust on the host, and removes the superseded `<previous_sha>` image tag.
"""
from __future__ import annotations

import fcntl
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(os.environ.get("AUTO_DEPLOY_REPO", "/home/labile/projects/maxwell"))
STATE_DIR = Path(os.environ.get("AUTO_DEPLOY_STATE_DIR", str(REPO / "self-improve")))
REMOTE = os.environ.get("AUTO_DEPLOY_REMOTE", "origin")
BRANCH = os.environ.get("AUTO_DEPLOY_BRANCH", "main")
IMAGE_REPO = os.environ.get("AUTO_DEPLOY_IMAGE_REPO", "ghcr.io/labi-le/maxwell")
COMPOSE_FILE = os.environ.get("AUTO_DEPLOY_COMPOSE_FILE", "compose.yaml")
SERVICE = os.environ.get("AUTO_DEPLOY_SERVICE", "maksut")
GIT_TIMEOUT = 60
PULL_TIMEOUT = 300
UP_TIMEOUT = 120
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def log(message: str) -> None:
    print(f"auto-deploy: {message}", file=sys.stderr, flush=True)


def run(
    args: list[str],
    timeout: int = GIT_TIMEOUT,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=str(cwd or REPO),
        env=env,
        timeout=timeout,
        text=True,
        capture_output=True,
        check=False,
    )


def git_stdout(args: list[str], cwd: Path | None = None) -> str | None:
    try:
        proc = run(["git", *args], timeout=GIT_TIMEOUT, cwd=cwd)
    except (OSError, subprocess.SubprocessError) as error:
        log(f"git {' '.join(args)} failed: {type(error).__name__}")
        return None
    if proc.returncode != 0:
        return None
    return (proc.stdout or "").strip()


def read_deployed_sha(state_dir: Path) -> str:
    marker = state_dir / "deployed_sha"
    try:
        value = marker.read_text().strip()
    except OSError:
        return ""
    return value if SHA_RE.fullmatch(value) else ""


def write_deployed_sha(state_dir: Path, sha: str) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    marker = state_dir / "deployed_sha"
    tmp = state_dir / f"deployed_sha.tmp-{os.getpid()}"
    tmp.write_text(f"{sha}\n")
    tmp.chmod(0o600)
    os.replace(tmp, marker)

def last_stderr_line(proc: subprocess.CompletedProcess[str]) -> str:
    lines = (proc.stderr or "").strip().splitlines()
    return lines[-1][:200] if lines else "no stderr"


def resolve_remote_target(repo: Path, remote: str, branch: str) -> tuple[str, str] | None:
    current_branch = git_stdout(["rev-parse", "--abbrev-ref", "HEAD"], cwd=repo)
    if current_branch != branch:
        log(f"skip: checkout is on branch {current_branch!r}, not {branch!r}")
        return None
    dirty = git_stdout(["status", "--porcelain", "--untracked-files=no"], cwd=repo)
    if dirty is None or dirty != "":
        log("skip: working tree has uncommitted tracked changes")
        return None
    if git_stdout(["fetch", "--quiet", remote, branch], cwd=repo) is None:
        log(f"git fetch {remote} {branch} failed")
        return None
    head_sha = git_stdout(["rev-parse", "HEAD"], cwd=repo)
    remote_sha = git_stdout(["rev-parse", f"{remote}/{branch}"], cwd=repo)
    if not head_sha or not remote_sha or not SHA_RE.fullmatch(remote_sha):
        log("unable to resolve HEAD or remote SHA")
        return None
    return head_sha, remote_sha


def advance_checkout(repo: Path, remote: str, branch: str, head_sha: str, remote_sha: str) -> bool:
    if head_sha == remote_sha:
        return True
    dirty = git_stdout(["status", "--porcelain", "--untracked-files=no"], cwd=repo)
    if dirty is None or dirty != "":
        log("skip: working tree became dirty before fast-forward")
        return False
    if git_stdout(["merge", "--ff-only", f"{remote}/{branch}"], cwd=repo) is None:
        log(f"fast-forward to {remote}/{branch} ({remote_sha[:7]}) failed")
        return False
    after_sha = git_stdout(["rev-parse", "HEAD"], cwd=repo)
    if after_sha != remote_sha:
        log(f"skip: checkout HEAD {after_sha!r} is ahead of {remote}/{branch} ({remote_sha[:7]})")
        return False
    log(f"fast-forwarded checkout {head_sha[:7]} -> {remote_sha[:7]}")
    return True

def pull_image(repo: Path, sha: str) -> bool:
    image = f"{IMAGE_REPO}:{sha}"
    try:
        pull_proc = run(["docker", "pull", "--quiet", image], timeout=PULL_TIMEOUT, cwd=repo)
    except (OSError, subprocess.SubprocessError) as error:
        log(f"docker pull {image} failed: {type(error).__name__}")
        return False
    if pull_proc.returncode != 0:
        log(f"docker pull {image} failed: {last_stderr_line(pull_proc)}")
        return False
    return True


def recreate_container(repo: Path, sha: str) -> bool:
    env = dict(os.environ)
    env["MAKSUT_IMAGE_TAG"] = sha
    try:
        up_proc = run(
            [
                "docker",
                "compose",
                "-f",
                COMPOSE_FILE,
                "up",
                "-d",
                "--no-build",
                SERVICE,
            ],
            timeout=UP_TIMEOUT,
            cwd=repo,
            env=env,
        )
    except (OSError, subprocess.SubprocessError) as error:
        log(f"docker compose up failed: {type(error).__name__}")
        return False
    if up_proc.returncode != 0:
        log(f"docker compose up failed: {last_stderr_line(up_proc)}")
        return False
    return True


def remove_image(repo: Path, sha: str) -> None:
    if not SHA_RE.fullmatch(sha):
        return
    try:
        proc = run(["docker", "image", "rm", f"{IMAGE_REPO}:{sha}"], timeout=GIT_TIMEOUT, cwd=repo)
    except (OSError, subprocess.SubprocessError) as error:
        log(f"docker image rm {sha[:7]} failed: {type(error).__name__}")
        return
    if proc.returncode != 0:
        log(f"docker image rm {sha[:7]} failed: {last_stderr_line(proc)}")


def deploy_once(
    repo: Path = REPO,
    state_dir: Path = STATE_DIR,
    remote: str = REMOTE,
    branch: str = BRANCH,
) -> bool:
    if (state_dir / "off").exists():
        return False
    resolved = resolve_remote_target(repo, remote, branch)
    if resolved is None:
        return False
    head_sha, target_sha = resolved
    previous_sha = read_deployed_sha(state_dir)
    if previous_sha == target_sha:
        if head_sha != target_sha:
            advance_checkout(repo, remote, branch, head_sha, target_sha)
        return False
    if not pull_image(repo, target_sha):
        return False
    if not advance_checkout(repo, remote, branch, head_sha, target_sha):
        return False
    if not recreate_container(repo, target_sha):
        return False
    write_deployed_sha(state_dir, target_sha)
    if previous_sha and previous_sha != target_sha:
        remove_image(repo, previous_sha)
    log(f"deployed {target_sha[:7]}")
    return True


def main() -> int:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    lock_path = STATE_DIR / "auto-deploy.lock"
    with lock_path.open("w") as lock_file:
        try:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return 0
        deploy_once()
    return 0


if __name__ == "__main__":
    sys.exit(main())
