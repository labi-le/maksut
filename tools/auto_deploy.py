#!/usr/bin/env python3
"""Host-side auto-deploy helper for Maksut.

Every deploy is derived from the remote branch alone: the container runs the
GHCR image for that commit, `compose.yaml` is read from that commit, and the
sources mounted for `inspect_code` are a `git archive` export of it. The
checkout is fast-forwarded only as a convenience, so uncommitted host edits can
neither block a deploy nor leak into the running container.
"""
from __future__ import annotations

import fcntl
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

REPO = Path(os.environ.get("AUTO_DEPLOY_REPO", "/home/labile/projects/maxwell"))
STATE_DIR = Path(os.environ.get("AUTO_DEPLOY_STATE_DIR", str(REPO / "self-improve")))
REMOTE = os.environ.get("AUTO_DEPLOY_REMOTE", "origin")
BRANCH = os.environ.get("AUTO_DEPLOY_BRANCH", "main")
IMAGE_REPO = os.environ.get("AUTO_DEPLOY_IMAGE_REPO", "ghcr.io/labi-le/maxwell")
COMPOSE_FILE = os.environ.get("AUTO_DEPLOY_COMPOSE_FILE", "compose.yaml")
SERVICE = os.environ.get("AUTO_DEPLOY_SERVICE", "maksut")
QDRANT_SERVICE = os.environ.get("AUTO_DEPLOY_QDRANT_SERVICE", "qdrant")
GIT_TIMEOUT = 60
PULL_TIMEOUT = 300
UP_TIMEOUT = 120
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
CODE_PATHS = ("src", "tools", "docs", "Cargo.toml", "README.md", "AGENTS.md")
# Written next to the checkout so relative bind mounts and `env_file` keep
# resolving against the repository root, then removed.
COMPOSE_OVERRIDE = ".auto-deploy.compose.yaml"
UNPUBLISHED_IMAGE_MARKERS = ("manifest unknown", "not found")
# `compose.yaml` names the project `maksut`, so the single service container keeps
# this name no matter which directory the file is read from.
CONTAINER = os.environ.get("AUTO_DEPLOY_CONTAINER", "maksut-maksut-1")
QDRANT_CONTAINER = os.environ.get("AUTO_DEPLOY_QDRANT_CONTAINER", "maksut-qdrant-1")
CODE_MOUNT = "/var/lib/maksut/code/src"
INSPECT_TIMEOUT = 30
# `docker inspect` without a template prints the whole config, including the
# token from `Config.Env`; the drift check needs only these four fields.
INSPECT_FORMAT = (
    '{"running":{{json .State.Running}},"status":{{json .State.Status}},'
    '"image":{{json .Config.Image}},"mounts":{{json .Mounts}}}'
)


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


def run_bytes(
    args: list[str], timeout: int = GIT_TIMEOUT, cwd: Path | None = None
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        args,
        cwd=str(cwd or REPO),
        timeout=timeout,
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


def last_stderr_line(proc: subprocess.CompletedProcess) -> str:
    stderr = proc.stderr or ""
    if isinstance(stderr, bytes):
        stderr = stderr.decode(errors="replace")
    lines = stderr.strip().splitlines()
    return lines[-1][:200] if lines else "no stderr"


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


def image_ref(sha: str) -> str:
    return f"{IMAGE_REPO}:{sha}"


def inspect_container(name: str = CONTAINER) -> dict | None:
    """Return `{running, status, image, mounts}` for `name`, None when absent."""
    try:
        proc = run(
            ["docker", "inspect", "--format", INSPECT_FORMAT, name], timeout=INSPECT_TIMEOUT
        )
    except (OSError, subprocess.SubprocessError) as error:
        log(f"docker inspect {name} failed: {type(error).__name__}")
        return None
    if proc.returncode != 0:
        detail = last_stderr_line(proc)
        if "no such object" not in detail.lower():
            log(f"docker inspect {name} failed: {detail}")
        return None
    try:
        container = json.loads(proc.stdout or "")
    except json.JSONDecodeError:
        log(f"docker inspect {CONTAINER} returned invalid JSON")
        return None
    return container if isinstance(container, dict) else None


def container_drift(sha: str, state_dir: Path) -> str | None:
    """Explain why the running service does not serve `sha`, or None when it does.

    `deployed_sha` only records what the last tick deployed; a manual
    `docker compose up` with another `MAKSUT_IMAGE_TAG` or `MAKSUT_CODE_DIR`
    leaves the marker lying, so the tick has to look at the container itself.
    """
    container = inspect_container()
    if container is None:
        return f"no {CONTAINER} container"
    if container.get("running") is not True or container.get("status") not in (None, "running"):
        return f"{CONTAINER} is not running"
    image = str(container.get("image") or "")
    if image != image_ref(sha):
        return f"container image {image or 'unknown'} != {image_ref(sha)}"
    expected = state_dir / "code" / sha / "src"
    source = ""
    for mount in container.get("mounts") or []:
        if isinstance(mount, dict) and mount.get("Destination") == CODE_MOUNT:
            source = str(mount.get("Source") or "")
            break
    if not source or os.path.realpath(source) != os.path.realpath(expected):
        return f"code mount {source or 'missing'} != {expected}"
    # The deploy brings qdrant up alongside maksut; a missing or stopped vector
    # service is drift even when the bot's own image and mount are correct.
    qdrant = inspect_container(QDRANT_CONTAINER)
    if qdrant is None:
        return f"no {QDRANT_CONTAINER} container"
    if qdrant.get("running") is not True or qdrant.get("status") not in (None, "running"):
        return f"{QDRANT_CONTAINER} is not running"
    return None


def status(state_dir: Path = STATE_DIR) -> int:
    """Print whether the running container serves the recorded `deployed_sha`."""
    sha = read_deployed_sha(state_dir)
    if not sha:
        print("unknown: no deployed_sha")
        return 0
    print(container_drift(sha, state_dir) or f"ok: running {image_ref(sha)}")
    return 0


def pull_image(sha: str) -> bool:
    image = image_ref(sha)
    try:
        proc = run(["docker", "pull", "--quiet", image], timeout=PULL_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        log(f"docker pull {image} failed: {type(error).__name__}")
        return False
    if proc.returncode != 0:
        detail = last_stderr_line(proc)
        if any(marker in detail.lower() for marker in UNPUBLISHED_IMAGE_MARKERS):
            log(f"image {sha[:7]} is not published yet: {detail}")
        else:
            log(f"docker pull {image} failed: {detail}")
        return False
    return True


def ensure_code_export(state_dir: Path, sha: str, repo: Path) -> Path | None:
    """Extract the tracked sources of `sha` for the `/var/lib/maksut/code` mounts."""
    target = state_dir / "code" / sha
    if (target / "src").is_dir():
        return target
    try:
        proc = run_bytes(["git", "archive", "--format=tar", sha, "--", *CODE_PATHS], cwd=repo)
    except (OSError, subprocess.SubprocessError) as error:
        log(f"git archive {sha[:7]} failed: {type(error).__name__}")
        return None
    if proc.returncode != 0:
        log(f"git archive {sha[:7]} failed: {last_stderr_line(proc)}")
        return None
    tmp = target.with_name(f".{sha}.tmp-{os.getpid()}")
    shutil.rmtree(tmp, ignore_errors=True)
    try:
        tmp.mkdir(parents=True)
        with tarfile.open(fileobj=io.BytesIO(proc.stdout or b"")) as archive:
            archive.extractall(tmp, filter="data")
        os.replace(tmp, target)
    except (OSError, tarfile.TarError) as error:
        shutil.rmtree(tmp, ignore_errors=True)
        log(f"code export {sha[:7]} failed: {type(error).__name__}")
        return None
    return target


def prune_code_exports(state_dir: Path, keep_sha: str) -> None:
    root = state_dir / "code"
    try:
        children = list(root.iterdir())
    except OSError:
        return
    for child in children:
        if child.name != keep_sha:
            shutil.rmtree(child, ignore_errors=True)


def compose_for(repo: Path, sha: str) -> tuple[Path | None, bool]:
    """Return the compose file of `sha`; the second value marks a temp copy."""
    try:
        proc = run_bytes(["git", "show", f"{sha}:{COMPOSE_FILE}"], cwd=repo)
    except (OSError, subprocess.SubprocessError) as error:
        log(f"skipping {sha[:7]}: git show {type(error).__name__}")
        return None, False
    if proc.returncode != 0 or not proc.stdout:
        log(f"skipping {sha[:7]}: {COMPOSE_FILE} is missing from the commit")
        return None, False
    override = repo / COMPOSE_OVERRIDE
    override.write_bytes(proc.stdout)
    return override, True


def fast_forward_checkout(repo: Path, remote: str, branch: str, target_sha: str) -> None:
    """Advance the checkout when that is free of surprises; never touch local edits."""
    current_branch = git_stdout(["rev-parse", "--abbrev-ref", "HEAD"], cwd=repo)
    if current_branch != branch:
        log(f"checkout left as is: on {current_branch!r}, not {branch!r}")
        return
    if git_stdout(["rev-parse", "HEAD"], cwd=repo) == target_sha:
        return
    dirty = git_stdout(["status", "--porcelain", "--untracked-files=no"], cwd=repo)
    if dirty is None or dirty != "":
        log("checkout left as is: uncommitted tracked changes")
        return
    if git_stdout(["merge", "--ff-only", f"{remote}/{branch}"], cwd=repo) is None:
        log(f"fast-forward to {remote}/{branch} failed")
    else:
        log(f"fast-forwarded checkout to {target_sha[:7]}")


def recreate_container(repo: Path, sha: str, code_dir: Path, compose_path: Path) -> bool:
    env = dict(os.environ)
    env["MAKSUT_IMAGE_TAG"] = sha
    env["MAKSUT_CODE_DIR"] = str(code_dir)
    try:
        proc = run(
            [
                "docker",
                "compose",
                "-f",
                str(compose_path),
                "up",
                "-d",
                "--no-build",
                SERVICE,
                QDRANT_SERVICE,
            ],
            timeout=UP_TIMEOUT,
            cwd=repo,
            env=env,
        )
    except (OSError, subprocess.SubprocessError) as error:
        log(f"docker compose up failed: {type(error).__name__}")
        return False
    if proc.returncode != 0:
        log(f"docker compose up failed: {last_stderr_line(proc)}")
        return False
    return True


def remove_image(sha: str) -> None:
    if not SHA_RE.fullmatch(sha):
        return
    try:
        proc = run(["docker", "image", "rm", image_ref(sha)], timeout=GIT_TIMEOUT)
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
    if git_stdout(["fetch", "--quiet", remote, branch], cwd=repo) is None:
        log(f"git fetch {remote} {branch} failed")
        return False
    target_sha = git_stdout(["rev-parse", f"{remote}/{branch}"], cwd=repo)
    if not target_sha or not SHA_RE.fullmatch(target_sha):
        log(f"unable to resolve {remote}/{branch}")
        return False
    previous_sha = read_deployed_sha(state_dir)
    if previous_sha == target_sha:
        fast_forward_checkout(repo, remote, branch, target_sha)
        drift = container_drift(target_sha, state_dir)
        if drift is None:
            return False
        log(f"reconciling {target_sha[:7]}: {drift}")
    if not pull_image(target_sha):
        return False
    code_dir = ensure_code_export(state_dir, target_sha, repo)
    if code_dir is None:
        log(f"skipping {target_sha[:7]}: no code export for the mounted sources")
        return False
    fast_forward_checkout(repo, remote, branch, target_sha)
    compose_path, temporary = compose_for(repo, target_sha)
    if compose_path is None:
        return False
    try:
        if not recreate_container(repo, target_sha, code_dir, compose_path):
            return False
    finally:
        if temporary:
            compose_path.unlink(missing_ok=True)
    if previous_sha != target_sha:
        write_deployed_sha(state_dir, target_sha)
    prune_code_exports(state_dir, target_sha)
    if previous_sha and previous_sha != target_sha:
        remove_image(previous_sha)
    if previous_sha == target_sha:
        log(f"reconciled container to {target_sha[:7]}")
    else:
        log(f"deployed {target_sha[:7]}")
    return True


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if args[:1] == ["--status"]:
        return status()
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
