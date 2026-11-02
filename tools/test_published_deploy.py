import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("published_deploy.py")
PUBLISHED = 'print("published")\n'
LOCAL = 'print("local")\n'


class PublishedDeployTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.tmp.name)
        self.repo = root / "repo"
        self.state = root / "state"
        self.runtime = root / "run"
        for path in (self.repo / "tools", self.state, self.runtime):
            path.mkdir(parents=True, exist_ok=True)
        self.git("init", "-b", "main")
        self.write_published(PUBLISHED)
        self.git("add", "tools/auto_deploy.py")
        self.git(
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "commit",
            "-q",
            "-m",
            "seed",
        )
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        proc = subprocess.run(
            ["git", "-C", str(self.repo), *args], capture_output=True, text=True, check=False
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def write_published(self, source):
        (self.repo / "tools" / "auto_deploy.py").write_text(source)

    def run_shim(self):
        env = dict(os.environ)
        env["AUTO_DEPLOY_REPO"] = str(self.repo)
        env["AUTO_DEPLOY_STATE_DIR"] = str(self.state)
        env["XDG_RUNTIME_DIR"] = str(self.runtime)
        return subprocess.run(
            [sys.executable, str(SCRIPT)], capture_output=True, text=True, env=env, check=False
        )

    def test_runs_the_published_script_not_the_working_copy(self):
        self.write_published(LOCAL)
        proc = self.run_shim()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.strip(), "published")

    def test_kill_switch_stops_before_running_anything(self):
        (self.state / "off").write_text("")
        proc = self.run_shim()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.strip(), "")

    def test_missing_remote_ref_fails_loudly(self):
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        proc = self.run_shim()
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(proc.stdout.strip(), "")
        self.assertIn("git show failed", proc.stderr)


if __name__ == "__main__":
    unittest.main()
