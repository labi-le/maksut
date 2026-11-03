"""Tests for the live e2e driver in `tools/e2e_telegram.py`.

The transport tests run against an in-process fake MCP server, so the whole
client, the polling loop and the checks are exercised without a Telegram
session. The live case at the bottom runs only where `self-improve/e2e.json`
and the real telegram-mcp are present.
"""

import json
import os
import pathlib
import socket
import stat
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest import mock

try:
    from tools import e2e_telegram
except ModuleNotFoundError:
    import e2e_telegram


class FakeChat:
    """In-memory Telegram chat behind a fake MCP server."""

    def __init__(
        self,
        *,
        tools=None,
        sse=False,
        reply=True,
        reply_text="pong",
        media=None,
        sticker=None,
        download=None,
    ):
        self.tools = tools if tools is not None else ["send_message", "list_messages", "download_media"]
        self.sse = sse
        self.reply = reply
        self.reply_text = reply_text
        self.media = media
        self.sticker = sticker
        self.download = download
        self.calls: list[tuple[str, dict]] = []
        self.messages: list[dict] = []
        self._next_id = 100
        self._lock = threading.Lock()

    def post(self, payload: dict) -> tuple[int, dict | None]:
        method = payload.get("method")
        if method == "initialize":
            return 200, {"jsonrpc": "2.0", "id": payload["id"], "result": {"protocolVersion": "2025-11-25"}}
        if method == "notifications/initialized":
            return 202, None
        if method == "tools/list":
            return 200, {
                "jsonrpc": "2.0",
                "id": payload["id"],
                "result": {"tools": [{"name": name} for name in self.tools]},
            }
        if method == "tools/call":
            name = payload["params"]["name"]
            arguments = payload["params"].get("arguments") or {}
            self.calls.append((name, arguments))
            return 200, {
                "jsonrpc": "2.0",
                "id": payload["id"],
                "result": {"content": [{"type": "text", "text": self._call(name, arguments)}]},
            }
        return 200, {
            "jsonrpc": "2.0",
            "id": payload.get("id"),
            "error": {"code": -32601, "message": f"unknown method {method}"},
        }

    def _message(self, **fields) -> dict:
        self._next_id += 1
        record = {"id": self._next_id}
        record.update(fields)
        self.messages.insert(0, record)
        return record

    def _call(self, name: str, arguments: dict) -> str:
        with self._lock:
            if name == "send_message":
                text = arguments["message"]
                self._message(sender="owner", username="owner", out=True, text=text)
                if self.reply and self.media and self.media in text:
                    self._message(sender="maksut", username="maksut_bot", text="", media="Video")
                elif self.reply and self.sticker and self.sticker in text:
                    self._message(sender="maksut", username="maksut_bot", media="Sticker")
                elif self.reply:
                    self._message(sender="maksut", username="maksut_bot", text=self.reply_text)
                return "Message sent."
            if name == "list_messages":
                return json.dumps({"results": self.messages})
            if name == "download_media":
                return f"Media downloaded to {self.download}."
            return f"unknown tool {name}"


class FakeServer:
    def __init__(self, chat: FakeChat):
        chat_state = chat
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802 - BaseHTTPRequestHandler API
                length = int(self.headers.get("Content-Length") or 0)
                payload = json.loads(self.rfile.read(length) or b"{}")
                status, body = chat_state.post(payload)
                if status == 202 or body is None:
                    self.send_response(202)
                    self.send_header("Mcp-Session-Id", "fake-session")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                encoded = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Mcp-Session-Id", "fake-session")
                if chat_state.sse:
                    self.send_header("Content-Type", "text/event-stream")
                    encoded = b"event: message\ndata: " + encoded + b"\n\n"
                else:
                    self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, fmt, *args):
                del fmt, args

        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=5)

    @property
    def url(self):
        host, port = self.httpd.server_address[:2]
        return f"http://{host}:{port}/mcp"


def config(url: str, **overrides) -> e2e_telegram.Config:
    values = {
        "chat_id": "-100123",
        "bot_username": "maksut_bot",
        "mcp_url": url,
        "prompt": "ping",
        "timeout_secs": 1.0,
        "poll_secs": 0.02,
        "media_url": "",
        "media_timeout_secs": 1.0,
        "sticker_prompt": "",
        "require": False,
    }
    values.update(overrides)
    return e2e_telegram.Config(**values)


def statuses(results):
    return {result.name: result.status for result in results}


class ConfigTests(unittest.TestCase):
    def test_missing_sources_yield_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(e2e_telegram.load_config(pathlib.Path(tmp) / "absent.json", environ={}))

    def test_file_and_env_overrides(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(
                json.dumps({"chat_id": -1001, "bot_username": "@Maksut_bot", "timeout_secs": 30}),
                encoding="utf-8",
            )
            loaded = e2e_telegram.load_config(
                path, environ={e2e_telegram.MEDIA_ENV: "https://example.com/clip"}
            )
        self.assertEqual(loaded.chat_id, "-1001")
        self.assertEqual(loaded.bot_username, "Maksut_bot")
        self.assertEqual(loaded.timeout_secs, 30.0)
        self.assertEqual(loaded.media_url, "https://example.com/clip")
        self.assertEqual(loaded.mcp_url, e2e_telegram.DEFAULT_MCP_URL)

    def test_unknown_key_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(
                json.dumps({"chat_id": 1, "bot_username": "b", "chat": 2}), encoding="utf-8"
            )
            with self.assertRaises(e2e_telegram.ConfigError) as caught:
                e2e_telegram.load_config(path, environ={})
        self.assertIn("chat", str(caught.exception))

    def test_bot_username_is_required(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(json.dumps({"chat_id": 1}), encoding="utf-8")
            with self.assertRaises(e2e_telegram.ConfigError):
                e2e_telegram.load_config(path, environ={})

    def test_timeout_is_bounded(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(
                json.dumps({"chat_id": 1, "bot_username": "b", "timeout_secs": 0}), encoding="utf-8"
            )
            with self.assertRaises(e2e_telegram.ConfigError):
                e2e_telegram.load_config(path, environ={})

    def test_non_http_mcp_url_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(
                json.dumps({"chat_id": 1, "bot_username": "b", "mcp_url": "file:///tmp/x"}),
                encoding="utf-8",
            )
            with self.assertRaises(e2e_telegram.ConfigError):
                e2e_telegram.load_config(path, environ={})

    def test_require_env(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(json.dumps({"chat_id": 1, "bot_username": "b"}), encoding="utf-8")
            loaded = e2e_telegram.load_config(path, environ={e2e_telegram.REQUIRE_ENV: "true"})
        self.assertTrue(loaded.require)

    def test_shared_mcp_url_is_the_default(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(json.dumps({"chat_id": 1, "bot_username": "b"}), encoding="utf-8")
            loaded = e2e_telegram.load_config(
                path, environ={"TELEGRAM_MCP_URL": "http://192.168.1.2:8932/mcp"}
            )
        self.assertEqual(loaded.mcp_url, "http://192.168.1.2:8932/mcp")


class ParsingTests(unittest.TestCase):
    def test_sse_events_are_split_and_parsed(self):
        first = {"jsonrpc": "2.0", "id": 1, "result": {"ok": True}}
        second = {"jsonrpc": "2.0", "id": 2, "result": {"ok": False}}
        stream = f"event: message\ndata: {json.dumps(first)}\n\nevent: message\ndata: {json.dumps(second)}\n\n"
        self.assertEqual(e2e_telegram._sse_messages(stream), [first, second])

    def test_non_json_event_is_ignored(self):
        self.assertEqual(e2e_telegram._sse_messages("data: keep-alive\n\n"), [])

    def test_records_accept_empty_chat(self):
        self.assertEqual(e2e_telegram._records("No messages found matching the criteria."), [])

    def test_records_reject_garbage(self):
        with self.assertRaises(e2e_telegram.Failed):
            e2e_telegram._records("Chat not found")

    def test_mention_is_added_once(self):
        self.assertEqual(e2e_telegram.with_mention("ping", "bot"), "@bot ping")
        self.assertEqual(e2e_telegram.with_mention("@bot ping", "bot"), "@bot ping")

    def test_literal_markup_is_detected(self):
        self.assertEqual(e2e_telegram.leaked_markup("привет <b>мир</b>"), "<b>")
        self.assertIsNone(e2e_telegram.leaked_markup("привет мир"))

    def test_secret_patterns_are_detected(self):
        self.assertIsNotNone(
            e2e_telegram.leaked_secret("token 123456789:AAHfoo_bar-bazqux123456789012345678")
        )
        self.assertIsNotNone(e2e_telegram.leaked_secret("BOT_TOKEN=secret"))
        self.assertIsNone(e2e_telegram.leaked_secret("обычный ответ"))

    def test_bot_filter_ignores_own_and_human_messages(self):
        self.assertFalse(e2e_telegram._is_bot({"username": "maksut_bot", "out": True}, "maksut_bot"))
        self.assertFalse(e2e_telegram._is_bot({"username": "human"}, "maksut_bot"))
        self.assertTrue(e2e_telegram._is_bot({"username": "Maksut_Bot"}, "maksut_bot"))


class RunnerTests(unittest.TestCase):
    def test_happy_path_uses_the_mention(self):
        chat = FakeChat()
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(config(server.url)).run()
        self.assertEqual(
            statuses(results),
            {
                "mcp": "pass",
                "reply": "pass",
                "formatting": "pass",
                "secret-leak": "pass",
            },
        )
        sent = [arguments for name, arguments in chat.calls if name == "send_message"]
        self.assertEqual(sent[0]["message"], "@maksut_bot ping")
        self.assertEqual(sent[0]["chat_id"], "-100123")

    def test_sse_transport_is_supported(self):
        chat = FakeChat(sse=True)
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(config(server.url)).run()
        self.assertEqual(statuses(results)["reply"], "pass")

    def test_missing_tool_fails_the_preflight(self):
        chat = FakeChat(tools=["send_message"])
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(config(server.url)).run()
        self.assertEqual(statuses(results), {"mcp": "fail"})
        self.assertIn("list_messages", results[0].detail)

    def test_silent_bot_fails_the_reply_check(self):
        chat = FakeChat(reply=False)
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(config(server.url, timeout_secs=0.3)).run()
        self.assertEqual(statuses(results), {"mcp": "pass", "reply": "fail"})
        self.assertIn("no bot reply", results[1].detail)

    def test_unreachable_mcp_is_a_skip(self):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        results = e2e_telegram.Runner(config(f"http://127.0.0.1:{port}/mcp")).run()
        self.assertEqual(statuses(results), {"mcp": "skip"})

    def test_leaked_markup_fails_formatting(self):
        chat = FakeChat(reply_text="<b>pong</b>")
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(config(server.url)).run()
        self.assertEqual(statuses(results)["formatting"], "fail")
        self.assertIn("<b>", results[2].detail)

    def test_media_requires_a_media_message(self):
        chat = FakeChat(media="https://example.com/clip")
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(
                config(server.url, media_url="https://example.com/clip"), ffprobe="/nonexistent"
            ).run()
        self.assertEqual(statuses(results)["media"], "pass")
        self.assertEqual(statuses(results)["media-audio"], "skip")

    def test_media_timeout_fails(self):
        chat = FakeChat(media=None)
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(
                config(server.url, media_url="https://example.com/clip", media_timeout_secs=0.3)
            ).run()
        self.assertEqual(statuses(results)["media"], "fail")

    def test_audio_track_is_verified_by_ffprobe(self):
        with tempfile.TemporaryDirectory() as tmp:
            media = pathlib.Path(tmp) / "clip.mp4"
            media.write_bytes(b"stub")
            probe = pathlib.Path(tmp) / "ffprobe"
            probe.write_text("#!/bin/sh\necho video\necho audio\n", encoding="utf-8")
            probe.chmod(probe.stat().st_mode | stat.S_IXUSR)
            chat = FakeChat(media="https://example.com/clip", download=str(media))
            with FakeServer(chat) as server:
                results = e2e_telegram.Runner(
                    config(server.url, media_url="https://example.com/clip"), ffprobe=str(probe)
                ).run()
        self.assertEqual(statuses(results)["media-audio"], "pass")

    def test_video_without_audio_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            media = pathlib.Path(tmp) / "clip.mp4"
            media.write_bytes(b"stub")
            probe = pathlib.Path(tmp) / "ffprobe"
            probe.write_text("#!/bin/sh\necho video\n", encoding="utf-8")
            probe.chmod(probe.stat().st_mode | stat.S_IXUSR)
            chat = FakeChat(media="https://example.com/clip", download=str(media))
            with FakeServer(chat) as server:
                results = e2e_telegram.Runner(
                    config(server.url, media_url="https://example.com/clip"), ffprobe=str(probe)
                ).run()
        self.assertEqual(statuses(results)["media-audio"], "fail")

    def test_sticker_prompt_is_checked(self):
        chat = FakeChat(sticker="пришли стикер")
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(
                config(server.url, sticker_prompt="пришли стикер")
            ).run()
        self.assertEqual(statuses(results)["sticker"], "pass")

    def test_sticker_timeout_fails(self):
        chat = FakeChat(sticker=None)
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(
                config(server.url, sticker_prompt="пришли стикер", timeout_secs=0.3)
            ).run()
        self.assertEqual(statuses(results)["sticker"], "fail")

    def test_new_messages_before_the_marker_are_ignored(self):
        chat = FakeChat(reply=False)
        chat.messages.append(
            {"id": 1, "sender": "maksut", "username": "maksut_bot", "text": "old answer"}
        )
        with FakeServer(chat) as server:
            results = e2e_telegram.Runner(config(server.url, timeout_secs=0.3)).run()
        self.assertEqual(statuses(results)["reply"], "fail")


class CliTests(unittest.TestCase):
    def test_absent_config_skips(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            with mock.patch.dict(os.environ, {e2e_telegram.CONFIG_ENV: str(path)}, clear=False):
                self.assertEqual(e2e_telegram.main([]), 0)

    def test_absent_config_fails_when_required(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            with mock.patch.dict(os.environ, {e2e_telegram.CONFIG_ENV: str(path)}, clear=False):
                self.assertEqual(e2e_telegram.main(["--require"]), 1)

    def test_invalid_config_exits_two(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text("{not json", encoding="utf-8")
            with mock.patch.dict(os.environ, {e2e_telegram.CONFIG_ENV: str(path)}, clear=False):
                self.assertEqual(e2e_telegram.main([]), 2)

    def test_failing_check_exits_one(self):
        chat = FakeChat(tools=["send_message"])
        with FakeServer(chat) as server, tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(
                json.dumps({"chat_id": -100123, "bot_username": "maksut_bot", "mcp_url": server.url}),
                encoding="utf-8",
            )
            with mock.patch.dict(os.environ, {e2e_telegram.CONFIG_ENV: str(path)}, clear=False):
                self.assertEqual(e2e_telegram.main([]), 1)

    def test_require_turns_skip_into_failure(self):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "e2e.json"
            path.write_text(
                json.dumps(
                    {
                        "chat_id": -100123,
                        "bot_username": "maksut_bot",
                        "mcp_url": f"http://127.0.0.1:{port}/mcp",
                        "require": True,
                    }
                ),
                encoding="utf-8",
            )
            with mock.patch.dict(os.environ, {e2e_telegram.CONFIG_ENV: str(path)}, clear=False):
                self.assertEqual(e2e_telegram.main([]), 1)


def live_config() -> e2e_telegram.Config | None:
    try:
        return e2e_telegram.load_config()
    except e2e_telegram.ConfigError:
        return None


LIVE = live_config()


@unittest.skipUnless(LIVE is not None, "no self-improve/e2e.json: the Telegram MCP is unavailable here")
class LiveTelegramTests(unittest.TestCase):
    def test_feature_works_on_the_real_chat(self):
        results = e2e_telegram.Runner(LIVE).run()
        failures = [f"{result.name}: {result.detail}" for result in results if result.status == "fail"]
        print(e2e_telegram.render(results), file=sys.stderr)
        self.assertEqual(failures, [])


if __name__ == "__main__":
    unittest.main()
