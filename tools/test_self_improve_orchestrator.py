import importlib.util
import json
import os
import pathlib
import tempfile
import unittest


def load(tmp):
    os.environ["SELF_IMPROVE_DIR"] = str(pathlib.Path(tmp) / "queue")
    os.environ["SELF_IMPROVE_REPO"] = str(pathlib.Path(tmp) / "repo")
    os.environ["OWNER_ID"] = "42"
    for name in (
        "queue/inbox",
        "queue/outbox",
        "queue/approvals",
        "queue/rejections",
        "queue/tmp",
    ):
        (pathlib.Path(tmp) / name).mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location(
        "orch", pathlib.Path(__file__).with_name("self_improve_orchestrator.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ApprovalTests(unittest.TestCase):
    def test_propose_writes_proposed_without_creating_issue(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.create_issue = lambda *args: self.fail(
                "create_issue must not run before owner approval"
            )
            module.propose(
                {
                    "id": "plan-1",
                    "goal": "добавь голосование",
                    "stage": "plan",
                    "chat_id": 10,
                    "source_chat_id": -100,
                    "proposed_by": 7,
                    "proposed_by_name": "bob",
                    "requested_by": 42,
                }
            )
            value = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-1.json").read_text()
            )
            self.assertEqual(value["status"], "proposed")
            self.assertEqual(value["goal"], "добавь голосование")
            self.assertEqual(value["issue_url"], "")
            self.assertEqual(value["proposed_by"], 7)
            self.assertEqual(value["proposed_by_name"], "bob")

    def test_approval_creates_github_issue_and_marks_issue_created(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {
                    "id": "plan-2",
                    "chat_id": 10,
                    "goal": "сделай кнопку",
                    "proposed_by": 7,
                    "proposed_by_name": "bob",
                    "requested_by": 42,
                },
                "proposed",
                "сделай кнопку",
                stage="plan",
            )
            (pathlib.Path(tmp) / "queue" / "approvals" / "plan-2.json").write_text(
                json.dumps({"id": "plan-2", "requested_by": 42})
            )
            module.create_issue = (
                lambda value: "https://github.com/labi-le/maxwell/issues/15"
            )
            module.process_approvals()
            value = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-2.json").read_text()
            )
            self.assertEqual(value["status"], "issue_created")
            self.assertEqual(
                value["issue_url"], "https://github.com/labi-le/maxwell/issues/15"
            )
            self.assertFalse(
                (pathlib.Path(tmp) / "queue" / "approvals" / "plan-2.json").exists()
            )

    def test_approval_marks_failed_when_issue_creation_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {"id": "plan-fail", "chat_id": 10, "goal": "x", "requested_by": 42},
                "proposed",
                "x",
            )
            (pathlib.Path(tmp) / "queue" / "approvals" / "plan-fail.json").write_text(
                json.dumps({"id": "plan-fail", "requested_by": 42})
            )
            module.create_issue = lambda value: ""
            module.process_approvals()
            value = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-fail.json").read_text()
            )
            self.assertEqual(value["status"], "failed")


    def test_approval_marks_failed_when_gh_times_out(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {"id": "plan-timeout", "chat_id": 10, "goal": "x", "requested_by": 42},
                "proposed",
                "x",
            )
            (pathlib.Path(tmp) / "queue" / "approvals" / "plan-timeout.json").write_text(
                json.dumps({"id": "plan-timeout", "requested_by": 42})
            )

            def timeout_run(*args, **kwargs):
                raise module.subprocess.TimeoutExpired(cmd=["gh"], timeout=60)

            module.run = timeout_run
            module.process_approvals()
            value = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-timeout.json").read_text()
            )
            self.assertEqual(value["status"], "failed")
    def test_rejection_is_terminal_without_creating_issue(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {
                    "id": "plan-3",
                    "chat_id": 10,
                    "goal": "план",
                    "requested_by": 42,
                    "created_at": 1700000000,
                    "parent": "plan-0",
                },
                "proposed",
                "план",
            )
            (pathlib.Path(tmp) / "queue" / "rejections" / "plan-3.json").write_text(
                "reject"
            )
            module.create_issue = lambda *args: self.fail(
                "create_issue must not run on rejection"
            )
            module.process_approvals()
            value = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-3.json").read_text()
            )
            self.assertEqual(value["status"], "rejected")
            self.assertEqual(value["goal"], "план")
            self.assertEqual(value["chat_id"], 10)
            self.assertEqual(value["created_at"], 1700000000)
            self.assertEqual(value["requested_by"], 42)
            self.assertEqual(value["parent"], "plan-0")
            self.assertFalse(
                (pathlib.Path(tmp) / "queue" / "rejections" / "plan-3.json").exists()
            )

    def test_invalid_approval_marker_is_ignored_and_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {"id": "plan-5", "chat_id": 10, "goal": "x"}, "proposed", "x"
            )
            marker = pathlib.Path(tmp) / "queue" / "approvals" / "plan-5.json"
            marker.write_text(json.dumps({"id": "plan-5", "requested_by": 1}))
            module.create_issue = lambda *args: self.fail(
                "invalid marker must not create issue"
            )
            module.process_approvals()
            self.assertFalse(marker.exists())
            value = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-5.json").read_text()
            )
            self.assertEqual(value["status"], "proposed")

    def test_create_issue_returns_the_url_and_cleans_temp_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            calls = []
            captured_body = []

            def fake_run(args, **kwargs):
                calls.append(args)
                body_file = pathlib.Path(args[args.index("--body-file") + 1])
                captured_body.append(body_file.read_text())
                return type(
                    "P",
                    (),
                    {
                        "returncode": 0,
                        "stdout": "https://github.com/labi-le/maxwell/issues/9\n",
                    },
                )()

            module.run = fake_run
            url = module.create_issue(
                {
                    "id": "propose-bob",
                    "goal": "добавь кнопку\nвторая строка",
                    "proposed_by": 7,
                    "proposed_by_name": "bob",
                    "username": "bob",
                    "created_at": 1700000000,
                    "feedback": "сделай синей",
                }
            )
            self.assertEqual(url, "https://github.com/labi-le/maxwell/issues/9")
            self.assertIn("--body-file", calls[0])
            self.assertIn("добавь кнопку", captured_body[0])
            self.assertIn("предложение от @bob", captured_body[0])
            self.assertIn("время: 2023-11-14 22:13 UTC", captured_body[0])
            self.assertNotIn("Предложил:", captured_body[0])
            self.assertIn("Уточнение: сделай синей", captured_body[0])
            title = calls[0][calls[0].index("--title") + 1]
            self.assertEqual(title, "добавь кнопку вторая строка")
            self.assertNotIn("Maksut:", title)
            self.assertNotIn("\n", title)
            self.assertFalse(
                (pathlib.Path(tmp) / "queue" / "tmp" / "propose-bob-issue.md").exists()
            )

    def test_approval_title_from_the_bot_wins_over_the_goal(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {
                    "id": "propose-bob",
                    "chat_id": 10,
                    "goal": "сделай кнопку отмены в меню заявок",
                    "proposed_by": 7,
                    "username": "bob",
                    "requested_by": 42,
                    "created_at": 1700000000,
                },
                "proposed",
                "ok",
            )
            (pathlib.Path(tmp) / "queue" / "approvals" / "propose-bob.json").write_text(
                json.dumps(
                    {
                        "id": "propose-bob",
                        "requested_by": 42,
                        "title": "кнопка отмены",
                    }
                )
            )
            captured = []
            module.create_issue = lambda value: captured.append(dict(value)) or ""
            module.process_approvals()
            self.assertEqual(len(captured), 1)
            self.assertEqual(captured[0]["title"], "кнопка отмены")
            self.assertEqual(captured[0]["username"], "bob")

    def test_issue_title_falls_back_to_a_short_goal(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            title = module.issue_title(
                {"goal": "  первые   слова заявки " * 20}
            )
            self.assertLessEqual(len(title), 70)
            self.assertFalse(title.endswith(" "))
            self.assertTrue(title.startswith("первые слова заявки"))
            self.assertEqual(module.issue_title({"goal": "x", "title": " short "}), "short")

    def test_queue_sort_key_orders_priority_then_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            low = pathlib.Path(tmp) / "queue" / "outbox" / "low.json"
            high = pathlib.Path(tmp) / "queue" / "outbox" / "high.json"
            low.write_text(json.dumps({"priority": 0}))
            high.write_text(json.dumps({"priority": 5}))
            ordered = sorted([low, high], key=module.queue_sort_key)
            self.assertEqual([path.name for path in ordered], ["high.json", "low.json"])

    def test_status_preserves_priority_and_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            module.status(
                {
                    "id": "plan-1",
                    "priority": 7,
                    "created_at": 1700000000,
                    "requested_by": 42,
                    "parent": "plan-0",
                    "feedback": "сделай иначе",
                },
                "proposed",
                "ok",
            )
            stored = json.loads(
                (pathlib.Path(tmp) / "queue" / "outbox" / "plan-1.json").read_text()
            )
            self.assertEqual(stored["priority"], 7)
            self.assertEqual(stored["created_at"], 1700000000)
            self.assertGreaterEqual(stored["updated_at"], stored["created_at"])
            self.assertEqual(stored["requested_by"], 42)
            self.assertEqual(stored["parent"], "plan-0")
            self.assertEqual(stored["feedback"], "сделай иначе")


class SourceReloadTests(unittest.TestCase):
    def test_changed_source_requests_a_restart(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            script = pathlib.Path(tmp) / "orchestrator.py"
            script.write_text("POLL_SECONDS = 5\n")
            started = module.source_fingerprint(script)
            self.assertFalse(module.newer_source(started, script))
            script.write_text("POLL_SECONDS = 6\n")
            self.assertTrue(module.newer_source(started, script))

    def test_half_written_source_keeps_the_running_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            script = pathlib.Path(tmp) / "orchestrator.py"
            script.write_text("POLL_SECONDS = 5\n")
            started = module.source_fingerprint(script)
            script.write_text("def main(:\n")
            self.assertFalse(module.newer_source(started, script))

    def test_missing_source_keeps_the_running_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            module = load(tmp)
            started = module.source_fingerprint(pathlib.Path(tmp) / "gone.py")
            self.assertIsNone(started)
            self.assertFalse(module.newer_source(started, pathlib.Path(tmp) / "gone.py"))


if __name__ == "__main__":
    unittest.main()
