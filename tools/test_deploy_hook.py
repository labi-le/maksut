import hashlib
import hmac
import http.client
import importlib.util
import json
import pathlib
import sys
import threading
import unittest


def load_module():
    spec = importlib.util.spec_from_file_location(
        "deploy_hook", pathlib.Path(__file__).with_name("deploy_hook.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SECRET = b"test-secret"


def payload(
    *,
    action: str = "completed",
    name: str = "publish-image",
    conclusion: str = "success",
    branch: str = "main",
    event: str = "push",
    sha: str = "a" * 40,
) -> dict:
    return {
        "action": action,
        "workflow_run": {
            "name": name,
            "conclusion": conclusion,
            "head_branch": branch,
            "event": event,
            "head_sha": sha,
        },
    }


class Recorder:
    def __init__(self):
        self.triggers: list[str] = []

    def trigger(self, sha: str) -> bool:
        self.triggers.append(sha)
        return True


class HookServer:
    def __init__(self, module, runner, path="/webhook/maksut-deploy"):
        self.server = module.build_server(SECRET, runner, port=0, path=path)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def post(
        self,
        body: bytes,
        signature: str | None,
        path="/webhook/maksut-deploy",
        event: str | None = "workflow_run",
        user_agent: str = "GitHub-Hookshot/abc1234",
    ):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Content-Length": str(len(body)), "User-Agent": user_agent}
        if event is not None:
            headers["X-GitHub-Event"] = event
        if signature is not None:
            headers["X-Hub-Signature-256"] = signature
        connection.request("POST", path, body=body, headers=headers)
        response = connection.getresponse()
        response.read()
        status = response.status
        connection.close()
        return status

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def sign(body: bytes, secret: bytes = SECRET) -> str:
    return "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()


class DeployHookTests(unittest.TestCase):
    def setUp(self):
        self.module = load_module()
        self.runner = Recorder()
        self.hook = HookServer(self.module, self.runner)

    def tearDown(self):
        self.hook.close()

    def test_signed_publish_completion_triggers_one_deploy(self):
        body = json.dumps(payload()).encode()
        self.assertEqual(self.hook.post(body, sign(body)), 202)
        self.assertEqual(self.runner.triggers, ["a" * 40])

    def test_unsigned_and_foreign_secret_requests_are_rejected(self):
        body = json.dumps(payload()).encode()
        self.assertEqual(self.hook.post(body, None), 401)
        self.assertEqual(self.hook.post(body, sign(body, b"another-secret")), 401)
        self.assertEqual(self.hook.post(b"not json", "sha256=deadbeef"), 401)
        self.assertEqual(self.runner.triggers, [])

    def test_other_workflows_events_and_branches_are_ignored(self):
        for candidate in (
            payload(name="pr-review"),
            payload(action="requested"),
            payload(conclusion="failure"),
            payload(branch="feature/x"),
            payload(event="pull_request"),
        ):
            body = json.dumps(candidate).encode()
            self.assertEqual(self.hook.post(body, sign(body)), 204)
        self.assertEqual(self.runner.triggers, [])

    def test_wrong_path_and_method_are_refused(self):
        body = json.dumps(payload()).encode()
        self.assertEqual(self.hook.post(body, sign(body), path="/webhook/github"), 404)
        connection = http.client.HTTPConnection("127.0.0.1", self.hook.port, timeout=5)
        connection.request("GET", "/webhook/maksut-deploy")
        self.assertEqual(connection.getresponse().status, 405)
        connection.close()
        self.assertEqual(self.runner.triggers, [])

    def test_only_github_deliveries_are_served(self):
        body = json.dumps(payload()).encode()
        self.assertEqual(self.hook.post(body, sign(body), event=None), 403)
        self.assertEqual(self.hook.post(body, sign(body), event="push"), 403)
        self.assertEqual(self.hook.post(body, sign(body), user_agent="curl/8.5.0"), 403)
        self.assertEqual(self.runner.triggers, [])

    def test_ping_is_acknowledged_without_deploying(self):
        body = json.dumps({"zen": "Keep it logically awesome."}).encode()
        self.assertEqual(self.hook.post(body, sign(body), event="ping"), 204)
        self.assertEqual(self.hook.post(body, None, event="ping"), 401)
        self.assertEqual(self.runner.triggers, [])

    def test_signature_check_is_exact(self):
        body = b'{"action":"completed"}'
        self.assertTrue(self.module.signature_matches(SECRET, body, sign(body)))
        self.assertFalse(self.module.signature_matches(SECRET, body + b" ", sign(body)))
        self.assertFalse(self.module.signature_matches(SECRET, body, "sha1=abc"))
        self.assertFalse(self.module.signature_matches(SECRET, body, None))


class DeployRunnerTests(unittest.TestCase):
    def test_a_running_deploy_coalesces_follow_up_events(self):
        module = load_module()
        runner = module.DeployRunner(
            [sys.executable, "-c", "import time; time.sleep(0.4)"], timeout=10
        )
        self.assertTrue(runner.trigger("a" * 40))
        self.assertFalse(runner.trigger("b" * 40))
        self.assertIsNotNone(runner._thread)
        runner._thread.join(timeout=10)
        self.assertTrue(runner.trigger("c" * 40))


if __name__ == "__main__":
    unittest.main()
