#!/usr/bin/env python3
"""Drive the deployed bot through a real Telegram account to prove a feature works.

The Bot API never shows a bot the messages another bot sends, so the driver has to
be an MTProto user session. The `telegram-mcp` server holds one and exposes
send/list tools over streamable HTTP; this module is a thin MCP client plus the
checks, and it needs no bot token, api_id or session string of its own.

GitHub Actions cannot reach that server (it lives on the owner's host), so this
runs locally, during implementation, before a pull request. Configuration is
`self-improve/e2e.json` (see `example.e2e.json`), overridable through the
`MAKSUT_E2E_*` environment variables.

The MCP client below is deliberately stdlib-only, unlike `tools/build_sticker_index.py`
(which runs under `uv run --with mcp --with httpx`): this module is also a test in the
repository suite, and that suite runs under a bare `python3 -m unittest` on hosts that
have no third-party packages installed.

    python3 tools/e2e_telegram.py
    MAKSUT_E2E_REQUIRE=1 python3 tools/e2e_telegram.py

Exit codes: 0 passed or skipped, 1 a check failed, 2 the configuration is invalid.
A missing config or an unreachable MCP server is a skip because the module also
runs inside the repository suite on hosts without a session; `require` (config
key or `MAKSUT_E2E_REQUIRE=1`) turns that skip into a failure for the final run.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, replace
from pathlib import Path

CONFIG_ENV = "MAKSUT_E2E_CONFIG"
CHAT_ENV = "MAKSUT_E2E_CHAT_ID"
MCP_URL_ENV = "MAKSUT_E2E_MCP_URL"
BOT_ENV = "MAKSUT_E2E_BOT_USERNAME"
MEDIA_ENV = "MAKSUT_E2E_MEDIA_URL"
STICKER_ENV = "MAKSUT_E2E_STICKER_PROMPT"
REQUIRE_ENV = "MAKSUT_E2E_REQUIRE"

DEFAULT_CONFIG = Path("self-improve/e2e.json")
DEFAULT_MCP_URL = "http://127.0.0.1:8932/mcp"
# The pinned telegram-mcp supports 2024-11-05 .. 2026-07-28 and defaults to this one.
PROTOCOL_VERSION = "2025-11-25"

MAX_BODY = 4 << 20
MAX_TIMEOUT_SECS = 900.0
MAX_POLL_SECS = 60.0
MAX_POLL_LIMIT = 50
FFPROBE_TIMEOUT_SECS = 60
REQUEST_TIMEOUT_SECS = 20.0

SUPPORTED_KEYS = frozenset(
    {
        "chat_id",
        "bot_username",
        "mcp_url",
        "prompt",
        "timeout_secs",
        "poll_secs",
        "media_url",
        "media_timeout_secs",
        "sticker_prompt",
        "require",
    }
)

# Literal parse_mode markup in an outgoing message means the bot escaped or shipped
# its own tags instead of formatting them.
MARKUP_TAG = re.compile(
    r"</?(?:b|strong|i|em|u|s|code|pre|a|tg-emoji)(?:\s+[a-z-]+=\"[^\"]*\")*\s*/?>",
    re.IGNORECASE,
)
# Values that must never reach a chat: bot tokens and explicit secret assignments.
SECRET_PATTERNS = (
    re.compile(r"\b\d{8,10}:[A-Za-z0-9_-]{35}\b"),
    re.compile(r"\bBOT_TOKEN\s*="),
    re.compile(r"\bTELEGRAM_(?:API_HASH|SESSION)\s*="),
)
DOWNLOADED = re.compile(r"^Media downloaded to (.*?)\.?$", re.DOTALL)


class ConfigError(Exception):
    """The configuration file or the environment is unusable."""


class Unavailable(Exception):
    """The MCP server cannot be reached: a skip, not a failed check."""


class Failed(Exception):
    """A check found real breakage."""


@dataclass(frozen=True)
class Config:
    chat_id: str
    bot_username: str
    mcp_url: str = DEFAULT_MCP_URL
    prompt: str = "ping"
    timeout_secs: float = 120.0
    poll_secs: float = 2.0
    media_url: str = ""
    media_timeout_secs: float = 300.0
    sticker_prompt: str = ""
    require: bool = False


@dataclass(frozen=True)
class Result:
    name: str
    status: str
    detail: str


def _number(value: object, key: str, maximum: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ConfigError(f"{key} must be a number")
    if not 0 < float(value) <= maximum:
        raise ConfigError(f"{key} must be in (0, {maximum:g}]")
    return float(value)


def _text(value: object, key: str, *, required: bool) -> str:
    if not isinstance(value, str):
        raise ConfigError(f"{key} must be a string")
    stripped = value.strip()
    if required and not stripped:
        raise ConfigError(f"{key} must not be empty")
    return stripped


def _chat_id(value: object) -> str:
    if isinstance(value, bool):
        raise ConfigError("chat_id must be a number or a string")
    if isinstance(value, int):
        return str(value)
    return _text(value, "chat_id", required=True)


def _mcp_url(value: object) -> str:
    url = _text(value, "mcp_url", required=True)
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ConfigError("mcp_url must be an http(s) URL")
    return url


def _truthy(value: object) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "on"}
    raise ConfigError("require must be a boolean")


def build_config(raw: dict, default_mcp_url: str = DEFAULT_MCP_URL) -> Config:
    unknown = sorted(set(raw) - SUPPORTED_KEYS)
    if unknown:
        raise ConfigError(f"unknown key(s): {', '.join(unknown)}")
    if "chat_id" not in raw:
        raise ConfigError("chat_id is required")
    if "bot_username" not in raw:
        raise ConfigError("bot_username is required")
    return Config(
        chat_id=_chat_id(raw["chat_id"]),
        bot_username=_text(raw["bot_username"], "bot_username", required=True).lstrip("@"),
        mcp_url=_mcp_url(raw.get("mcp_url", default_mcp_url)),
        prompt=_text(raw.get("prompt", "ping"), "prompt", required=True),
        timeout_secs=_number(raw.get("timeout_secs", 120.0), "timeout_secs", MAX_TIMEOUT_SECS),
        poll_secs=_number(raw.get("poll_secs", 2.0), "poll_secs", MAX_POLL_SECS),
        media_url=_text(raw.get("media_url", ""), "media_url", required=False),
        media_timeout_secs=_number(
            raw.get("media_timeout_secs", 300.0), "media_timeout_secs", MAX_TIMEOUT_SECS
        ),
        sticker_prompt=_text(raw.get("sticker_prompt", ""), "sticker_prompt", required=False),
        require=_truthy(raw.get("require", False)),
    )


def load_config(path: Path | None = None, environ: dict | None = None) -> Config | None:
    """Return the configuration, or None when no source was provided at all."""
    env = os.environ if environ is None else environ
    configured = env.get(CONFIG_ENV)
    source = Path(configured) if configured else (path or DEFAULT_CONFIG)
    try:
        text = source.read_text(encoding="utf-8")
    except FileNotFoundError:
        text = None
    except OSError as error:
        raise ConfigError(f"cannot read {source}: {error.strerror}") from error
    raw: dict = {}
    if text is not None:
        try:
            loaded = json.loads(text)
        except ValueError as error:
            raise ConfigError(f"{source}: invalid JSON: {error}") from error
        if not isinstance(loaded, dict):
            raise ConfigError(f"{source}: expected a JSON object")
        raw = dict(loaded)
    overrides = (
        (CHAT_ENV, "chat_id", _chat_id),
        (MCP_URL_ENV, "mcp_url", _mcp_url),
        (BOT_ENV, "bot_username", lambda value: _text(value, "bot_username", required=True)),
        (MEDIA_ENV, "media_url", lambda value: _text(value, "media_url", required=False)),
        (STICKER_ENV, "sticker_prompt", lambda value: _text(value, "sticker_prompt", required=False)),
        (REQUIRE_ENV, "require", _truthy),
    )
    provided = text is not None
    for name, key, parse in overrides:
        value = env.get(name)
        if value is None or not value.strip():
            continue
        raw[key] = parse(value)
        provided = True
    if not provided:
        return None
    shared_url = (env.get("TELEGRAM_MCP_URL") or "").strip()
    return build_config(raw, _mcp_url(shared_url) if shared_url else DEFAULT_MCP_URL)


def _truncate(value: bytes | str, limit: int = 300) -> str:
    text = value.decode("utf-8", "replace") if isinstance(value, bytes) else value
    collapsed = " ".join(text.split())
    return collapsed[:limit] + ("…" if len(collapsed) > limit else "")


def _sse_messages(text: str) -> list[dict]:
    """JSON-RPC messages carried by server-sent events, one `data:` payload each."""
    events: list[str] = []
    pending: list[str] = []
    for line in text.splitlines():
        if not line.strip():
            if pending:
                events.append("\n".join(pending))
                pending = []
            continue
        if line.startswith("data:"):
            pending.append(line[5:].lstrip())
    if pending:
        events.append("\n".join(pending))
    messages = []
    for event in events:
        try:
            value = json.loads(event)
        except ValueError:
            continue
        if isinstance(value, dict):
            messages.append(value)
    return messages


class Mcp:
    """Minimal streamable-HTTP MCP client: initialize, tools/list, tools/call."""

    def __init__(self, url: str, *, timeout: float = REQUEST_TIMEOUT_SECS) -> None:
        self.url = url
        self.timeout = timeout
        self.session: str | None = None
        self._next_id = 0

    def initialize(self) -> None:
        self._rpc(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "maksut-e2e", "version": "1"},
            },
        )
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized"}, expect_reply=False)

    def tools(self) -> list[str]:
        result = self._rpc("tools/list", {})
        tools = result.get("tools") if isinstance(result, dict) else None
        if not isinstance(tools, list):
            raise Failed("MCP tools/list returned no tool array")
        return [tool["name"] for tool in tools if isinstance(tool, dict) and "name" in tool]

    def call(self, name: str, arguments: dict) -> str:
        result = self._rpc("tools/call", {"name": name, "arguments": arguments})
        if not isinstance(result, dict):
            raise Failed(f"{name} returned no result object")
        text = "\n".join(
            part.get("text", "")
            for part in result.get("content") or []
            if isinstance(part, dict) and part.get("type") == "text"
        )
        if result.get("isError"):
            raise Failed(f"{name} failed: {_truncate(text)}")
        return text

    def _rpc(self, method: str, params: dict) -> dict:
        self._next_id += 1
        message = self._post({"jsonrpc": "2.0", "id": self._next_id, "method": method, "params": params})
        if "error" in message:
            error = message["error"]
            detail = error.get("message") if isinstance(error, dict) else error
            raise Failed(f"MCP {method}: {_truncate(str(detail))}")
        result = message.get("result")
        if not isinstance(result, dict):
            raise Failed(f"MCP {method} returned no result object")
        return result

    def _post(self, payload: dict, *, expect_reply: bool = True) -> dict:
        body = json.dumps(payload).encode()
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": PROTOCOL_VERSION,
        }
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        request = urllib.request.Request(self.url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                self.session = response.headers.get("Mcp-Session-Id") or self.session
                content_type = (response.headers.get("Content-Type") or "").lower()
                raw = response.read(MAX_BODY + 1)
        except urllib.error.HTTPError as error:
            raise Unavailable(f"MCP answered HTTP {error.code}: {_truncate(error.read(MAX_BODY))}") from error
        except (urllib.error.URLError, OSError, TimeoutError) as error:
            raise Unavailable(f"MCP unreachable at {self.url}: {type(error).__name__}") from error
        if len(raw) > MAX_BODY:
            raise Unavailable(f"MCP response exceeded {MAX_BODY} bytes")
        if not expect_reply:
            return {}
        text = raw.decode("utf-8", "replace")
        if "text/event-stream" in content_type:
            messages = _sse_messages(text)
        else:
            try:
                parsed = json.loads(text)
            except ValueError as error:
                raise Unavailable(f"MCP returned non-JSON body: {_truncate(text)}") from error
            messages = [parsed] if isinstance(parsed, dict) else []
        wanted = payload["id"]
        for message in messages:
            if message.get("id") == wanted:
                return message
        raise Unavailable(f"MCP returned no reply to {payload['method']}")


def _records(text: str) -> list[dict]:
    """Parse a `list_messages` payload; an empty chat is a valid empty list."""
    if text.lstrip().startswith("No messages found"):
        return []
    try:
        payload = json.loads(text)
    except ValueError as error:
        raise Failed(f"list_messages returned non-JSON: {_truncate(text)}") from error
    results = payload.get("results") if isinstance(payload, dict) else None
    if not isinstance(results, list):
        raise Failed(f"list_messages returned no results array: {_truncate(text)}")
    return [record for record in results if isinstance(record, dict)]


def _is_bot(record: dict, username: str) -> bool:
    if record.get("out"):
        return False
    sender = (record.get("username") or "").lstrip("@").lower()
    return sender == username.lower()


def with_mention(text: str, bot_username: str) -> str:
    mention = f"@{bot_username}"
    if mention.lower() in text.lower():
        return text
    return f"{mention} {text}"


def leaked_markup(text: str) -> str | None:
    match = MARKUP_TAG.search(text)
    return match.group(0) if match else None


def leaked_secret(text: str) -> str | None:
    for pattern in SECRET_PATTERNS:
        match = pattern.search(text)
        if match:
            return pattern.pattern
    return None


class Runner:
    """Runs the checks in order and stops the dependent ones on a hard failure."""

    def __init__(self, config: Config, client: Mcp | None = None, *, ffprobe: str | None = None) -> None:
        self.config = config
        self.client = client or Mcp(config.mcp_url)
        self.ffprobe = shutil.which("ffprobe") if ffprobe is None else ffprobe

    def run(self) -> list[Result]:
        try:
            self.client.initialize()
            names = self.client.tools()
        except Unavailable as error:
            return [Result("mcp", "skip", str(error))]
        except Failed as error:
            return [Result("mcp", "fail", str(error))]
        missing = [name for name in ("send_message", "list_messages") if name not in names]
        if missing:
            return [Result("mcp", "fail", f"missing tool(s): {', '.join(missing)}")]
        results = [Result("mcp", "pass", f"{len(names)} tools exposed")]
        try:
            reply, elapsed = self._conversation()
        except (Unavailable, Failed) as error:
            results.append(Result("reply", "fail", str(error)))
            return results
        results.append(
            Result("reply", "pass", f"bot answered message {reply.get('id')} in {elapsed:.1f}s")
        )
        results.append(self._formatting(reply))
        results.append(self._secrets(reply))
        if self.config.media_url:
            results.extend(self._media())
        if self.config.sticker_prompt:
            results.append(self._sticker())
        return results

    def _conversation(self) -> tuple[dict, float]:
        marker = self._latest_id()
        started = time.monotonic()
        self.client.call(
            "send_message",
            {"chat_id": self.config.chat_id, "message": with_mention(self.config.prompt, self.config.bot_username)},
        )
        reply = self._await(marker, self.config.timeout_secs, "no bot reply")
        return reply, time.monotonic() - started

    def _latest_id(self) -> int:
        ids = [record.get("id") for record in self._messages() if isinstance(record.get("id"), int)]
        return max(ids, default=0)

    def _messages(self) -> list[dict]:
        return _records(
            self.client.call(
                "list_messages", {"chat_id": self.config.chat_id, "limit": MAX_POLL_LIMIT}
            )
        )

    def _await(self, after_id: int, timeout: float, description: str, match=None) -> dict:
        deadline = time.monotonic() + timeout
        last_error: str | None = None
        while True:
            try:
                for record in self._messages():
                    identifier = record.get("id")
                    if not isinstance(identifier, int) or identifier <= after_id:
                        continue
                    if not _is_bot(record, self.config.bot_username):
                        continue
                    if match is None or match(record):
                        return record
            except Unavailable as error:
                # A flapping MCP server is a retry; a malformed payload is not.
                last_error = str(error)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                detail = f"{description} within {timeout:g}s"
                raise Failed(f"{detail}: {last_error}" if last_error else detail)
            time.sleep(min(self.config.poll_secs, remaining))

    def _formatting(self, reply: dict) -> Result:
        text = reply.get("text") or ""
        if not text.strip():
            return Result("formatting", "fail", "bot message has no text")
        tag = leaked_markup(text)
        if tag:
            return Result("formatting", "fail", f"literal markup in the reply: {tag}")
        return Result("formatting", "pass", f"{len(text)} chars, no literal markup")

    def _secrets(self, reply: dict) -> Result:
        pattern = leaked_secret(reply.get("text") or "")
        if pattern:
            return Result("secret-leak", "fail", f"reply matched {pattern}")
        return Result("secret-leak", "pass", "no token-shaped value in the reply")

    def _media(self) -> list[Result]:
        try:
            marker = self._latest_id()
            self.client.call(
                "send_message", {"chat_id": self.config.chat_id, "message": self.config.media_url}
            )
            record = self._await(
                marker,
                self.config.media_timeout_secs,
                "bot delivered no media",
                match=lambda value: bool(value.get("media")),
            )
        except (Unavailable, Failed) as error:
            return [Result("media", "fail", str(error))]
        results = [Result("media", "pass", f"message {record.get('id')}: {record.get('media')}")]
        results.append(self._audio(record))
        return results

    def _audio(self, record: dict) -> Result:
        if self.ffprobe is None:
            return Result("media-audio", "skip", "ffprobe is not installed")
        try:
            downloaded = self.client.call(
                "download_media",
                {"chat_id": self.config.chat_id, "message_id": record.get("id")},
            )
        except (Unavailable, Failed) as error:
            return Result("media-audio", "skip", f"download unavailable: {error}")
        match = DOWNLOADED.match(downloaded.strip())
        if not match:
            return Result("media-audio", "skip", f"unexpected download reply: {_truncate(downloaded)}")
        path = Path(match.group(1))
        if not path.exists():
            return Result("media-audio", "skip", f"{path} is not reachable from this host")
        try:
            streams = self._stream_types(path)
        except Failed as error:
            return Result("media-audio", "fail", str(error))
        if "audio" not in streams:
            return Result("media-audio", "fail", f"no audio stream in {path.name}")
        return Result("media-audio", "pass", f"audio stream present in {path.name}")

    def _stream_types(self, path: Path) -> list[str]:
        try:
            proc = subprocess.run(
                [self.ffprobe, "-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0", str(path)],
                capture_output=True,
                timeout=FFPROBE_TIMEOUT_SECS,
                check=False,
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise Failed(f"ffprobe failed: {type(error).__name__}") from error
        if proc.returncode != 0:
            raise Failed(f"ffprobe exited {proc.returncode}: {_truncate(proc.stderr)}")
        return [line.strip() for line in proc.stdout.decode("utf-8", "replace").splitlines() if line.strip()]

    def _sticker(self) -> Result:
        try:
            marker = self._latest_id()
            self.client.call(
                "send_message",
                {"chat_id": self.config.chat_id, "message": with_mention(self.config.sticker_prompt, self.config.bot_username)},
            )
            record = self._await(
                marker,
                self.config.timeout_secs,
                "bot sent no sticker",
                match=lambda value: "sticker" in str(value.get("media", "")).lower(),
            )
        except (Unavailable, Failed) as error:
            return Result("sticker", "fail", str(error))
        return Result("sticker", "pass", f"message {record.get('id')}: {record.get('media')}")


def render(results: list[Result]) -> str:
    return "\n".join(f"{result.status.upper():4} {result.name}: {result.detail}" for result in results)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run the bot against a real Telegram chat.")
    parser.add_argument("--config", type=Path, help=f"default: {DEFAULT_CONFIG} or ${CONFIG_ENV}")
    parser.add_argument("--require", action="store_true", help="turn a skip into a failure")
    arguments = parser.parse_args(argv)
    try:
        config = load_config(arguments.config)
    except ConfigError as error:
        print(f"e2e telegram: {error}", file=sys.stderr)
        return 2
    if arguments.require and config is not None and not config.require:
        config = replace(config, require=True)
    if config is None:
        print("SKIP mcp: no config and no MAKSUT_E2E_CHAT_ID; nothing to run")
        return 1 if arguments.require else 0
    print(f"e2e telegram: chat {config.chat_id} via {config.mcp_url}")
    results = Runner(config).run()
    print(render(results))
    if any(result.status == "fail" for result in results):
        return 1
    if config.require and any(result.status == "skip" for result in results):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
