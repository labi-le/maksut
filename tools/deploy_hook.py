#!/usr/bin/env python3
"""GitHub webhook receiver that deploys Maksut when the image publish finishes.

The endpoint sits behind the host reverse proxy on one public path and accepts
nothing but a correctly signed `workflow_run` completion of `publish-image` on
the deploy branch. Deploying itself stays in `tools/auto_deploy.py`, which reads
only `origin/main`; this process only decides when to call it.
"""
from __future__ import annotations

import hashlib
import hmac
import json
import os
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(os.environ.get("AUTO_DEPLOY_REPO", "/home/labile/projects/maxwell"))
BRANCH = os.environ.get("AUTO_DEPLOY_BRANCH", "main")
SECRET_FILE = Path(
    os.environ.get(
        "MAKSUT_HOOK_SECRET_FILE", "/home/labile/projects/maxwell/.secrets/deploy-hook.secret"
    )
)
HOST = os.environ.get("MAKSUT_HOOK_HOST", "127.0.0.1")
PORT = int(os.environ.get("MAKSUT_HOOK_PORT", "6551"))
PATH = os.environ.get("MAKSUT_HOOK_PATH", "/webhook/maksut-deploy")
MAX_BODY_BYTES = 1 << 20
WORKFLOW = "publish-image"
DEPLOY_TIMEOUT = 900
DEPLOY_COMMAND = [sys.executable, str(REPO / "tools" / "published_deploy.py")]


def log(message: str) -> None:
    print(f"deploy-hook: {message}", file=sys.stderr, flush=True)


def read_secret(path: Path = SECRET_FILE) -> bytes | None:
    try:
        secret = path.read_bytes().strip()
    except OSError as error:
        log(f"cannot read the webhook secret: {type(error).__name__}")
        return None
    return secret or None


def github_delivery(event: str | None, user_agent: str | None) -> bool:
    """A real GitHub hook POST carries both headers; anything else is not ours."""
    return event in ("workflow_run", "ping") and (user_agent or "").startswith(
        "GitHub-Hookshot/"
    )


def signature_matches(secret: bytes, body: bytes, header: str | None) -> bool:
    if not header or not header.startswith("sha256="):
        return False
    expected = hmac.new(secret, body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, header[len("sha256=") :])


def deploy_sha(payload: object, branch: str = BRANCH) -> str | None:
    """Return the commit to deploy for a publish-image completion, else None."""
    if not isinstance(payload, dict):
        return None
    run = payload.get("workflow_run")
    if payload.get("action") != "completed" or not isinstance(run, dict):
        return None
    if run.get("name") != WORKFLOW or run.get("conclusion") != "success":
        return None
    if run.get("head_branch") != branch or run.get("event") not in ("push", "workflow_dispatch"):
        return None
    sha = run.get("head_sha")
    return sha if isinstance(sha, str) and len(sha) == 40 else None


class DeployRunner:
    """Runs one deploy at a time; a burst of events coalesces into the running one."""

    def __init__(self, command: list[str], timeout: int = DEPLOY_TIMEOUT):
        self.command = command
        self.timeout = timeout
        self._lock = threading.Lock()
        self._thread: threading.Thread | None = None

    def trigger(self, sha: str) -> bool:
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                log(f"deploy already running, {sha[:7]} coalesced")
                return False
            self._thread = threading.Thread(target=self._deploy, args=(sha,), daemon=True)
            self._thread.start()
            return True

    def _deploy(self, sha: str) -> None:
        log(f"deploying for {sha[:7]}")
        try:
            proc = subprocess.run(self.command, timeout=self.timeout, check=False)
        except (OSError, subprocess.SubprocessError) as error:
            log(f"deploy for {sha[:7]} failed: {type(error).__name__}")
            return
        log(f"deploy for {sha[:7]} exited {proc.returncode}")


class HookHandler(BaseHTTPRequestHandler):
    server_version = "maksut-deploy-hook"

    def do_POST(self) -> None:
        if self.path.split("?", 1)[0] != self.server.hook_path:  # type: ignore[attr-defined]
            self._reply(404)
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            self._reply(400)
            return
        if length <= 0 or length > MAX_BODY_BYTES:
            self._reply(413)
            return
        body = self.rfile.read(length)
        if not github_delivery(
            self.headers.get("X-GitHub-Event"), self.headers.get("User-Agent")
        ):
            log(f"rejected a non-GitHub delivery from {self.address_string()}")
            self._reply(403)
            return
        if not signature_matches(
            self.server.secret, body, self.headers.get("X-Hub-Signature-256")  # type: ignore[attr-defined]
        ):
            log(f"rejected an unsigned request from {self.address_string()}")
            self._reply(401)
            return
        if self.headers.get("X-GitHub-Event") == "ping":
            self._reply(204)
            return
        try:
            payload = json.loads(body)
        except ValueError:
            self._reply(400)
            return
        sha = deploy_sha(payload)
        if sha is None:
            self._reply(204)
            return
        self.server.runner.trigger(sha)  # type: ignore[attr-defined]
        self._reply(202)

    def do_GET(self) -> None:
        self._reply(405)

    def _reply(self, status: int) -> None:
        self.send_response(status)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, fmt: str, *args) -> None:
        log(f"{self.address_string()} {fmt % args}")


def build_server(
    secret: bytes,
    runner: DeployRunner,
    host: str = HOST,
    port: int = PORT,
    path: str = PATH,
) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer((host, port), HookHandler)
    server.secret = secret  # type: ignore[attr-defined]
    server.runner = runner  # type: ignore[attr-defined]
    server.hook_path = path  # type: ignore[attr-defined]
    return server


def main() -> int:
    secret = read_secret()
    if secret is None:
        return 1
    server = build_server(secret, DeployRunner(DEPLOY_COMMAND))
    log(f"listening on {HOST}:{PORT}{PATH}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        return 0
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
