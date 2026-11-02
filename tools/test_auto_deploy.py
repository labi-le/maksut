import importlib.util
import os
import pathlib
import subprocess
import tempfile
import unittest


def load_module(tmp: str):
    base = pathlib.Path(tmp)
    repo = base / "repo"
    state = base / "state"
    repo.mkdir(parents=True, exist_ok=True)
    state.mkdir(parents=True, exist_ok=True)
    os.environ["AUTO_DEPLOY_REPO"] = str(repo)
    os.environ["AUTO_DEPLOY_STATE_DIR"] = str(state)
    os.environ["AUTO_DEPLOY_REMOTE"] = "origin"
    os.environ["AUTO_DEPLOY_BRANCH"] = "main"
    os.environ["AUTO_DEPLOY_IMAGE_REPO"] = "ghcr.io/labi-le/maxwell"
    spec = importlib.util.spec_from_file_location(
        "auto_deploy", pathlib.Path(__file__).with_name("auto_deploy.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, repo, state


class AutoDeployTests(unittest.TestCase):
    def test_deploys_when_remote_advances_and_prunes_previous_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            prev_sha = "0" * 40
            old_sha = "a" * 40
            new_sha = "b" * 40
            module.write_deployed_sha(state, prev_sha)
            order = []

            merged = False

            def fake_run(args, timeout=60, cwd=None, env=None):
                nonlocal merged
                order.append(args)
                if args == ["git", "rev-parse", "--abbrev-ref", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout="main\n", stderr="")
                if args == ["git", "status", "--porcelain", "--untracked-files=no"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "fetch", "--quiet", "origin", "main"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "rev-parse", "HEAD"]:
                    current = new_sha if merged else old_sha
                    return subprocess.CompletedProcess(args, 0, stdout=f"{current}\n", stderr="")
                if args == ["git", "rev-parse", "origin/main"]:
                    return subprocess.CompletedProcess(args, 0, stdout=f"{new_sha}\n", stderr="")
                if args == ["docker", "pull", "--quiet", f"ghcr.io/labi-le/maxwell:{new_sha}"]:
                    return subprocess.CompletedProcess(args, 0, stdout=f"{new_sha}\n", stderr="")
                if args == ["git", "merge", "--ff-only", "origin/main"]:
                    merged = True
                    return subprocess.CompletedProcess(args, 0, stdout="Fast-forward\n", stderr="")
                if args[:3] == ["docker", "compose", "-f"]:
                    self.assertEqual(env.get("MAKSUT_IMAGE_TAG"), new_sha)
                    self.assertIn("--no-build", args)
                    return subprocess.CompletedProcess(args, 0, stdout="Started\n", stderr="")
                if args == ["docker", "image", "rm", f"ghcr.io/labi-le/maxwell:{prev_sha}"]:
                    return subprocess.CompletedProcess(args, 0, stdout="Untagged\n", stderr="")
                self.fail(f"unexpected command: {args}")

            module.run = fake_run
            self.assertTrue(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), new_sha)
            # Image pull happens before checkout fast-forward, and old image tag is removed after up.
            pull_idx = order.index(["docker", "pull", "--quiet", f"ghcr.io/labi-le/maxwell:{new_sha}"])
            ff_idx = order.index(["git", "merge", "--ff-only", "origin/main"])
            rm_idx = order.index(["docker", "image", "rm", f"ghcr.io/labi-le/maxwell:{prev_sha}"])
            self.assertLess(pull_idx, ff_idx)
            self.assertLess(ff_idx, rm_idx)

            # A second pass at the same SHA does not pull or recreate again.
            order.clear()

            def fake_run_same(args, timeout=60, cwd=None, env=None):
                order.append(args)
                if args == ["git", "rev-parse", "--abbrev-ref", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout="main\n", stderr="")
                if args == ["git", "status", "--porcelain", "--untracked-files=no"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "fetch", "--quiet", "origin", "main"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args in (
                    ["git", "rev-parse", "HEAD"],
                    ["git", "rev-parse", "origin/main"],
                ):
                    return subprocess.CompletedProcess(args, 0, stdout=f"{new_sha}\n", stderr="")
                self.fail(f"unexpected command on idempotent run: {args}")

            module.run = fake_run_same
            self.assertFalse(module.deploy_once(repo, state))
            self.assertFalse(any(cmd[0] == "docker" for cmd in order))

    def test_waits_without_fast_forwarding_or_updating_marker_when_pull_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            old_sha = "a" * 40
            new_sha = "c" * 40

            def fake_run(args, timeout=60, cwd=None, env=None):
                if args == ["git", "rev-parse", "--abbrev-ref", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout="main\n", stderr="")
                if args == ["git", "status", "--porcelain", "--untracked-files=no"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "fetch", "--quiet", "origin", "main"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "rev-parse", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout=f"{old_sha}\n", stderr="")
                if args == ["git", "rev-parse", "origin/main"]:
                    return subprocess.CompletedProcess(args, 0, stdout=f"{new_sha}\n", stderr="")
                if args == ["docker", "pull", "--quiet", f"ghcr.io/labi-le/maxwell:{new_sha}"]:
                    return subprocess.CompletedProcess(args, 1, stdout="", stderr="manifest unknown")
                self.fail(f"unexpected command (must not merge --ff-only before pull succeeds): {args}")

            module.run = fake_run
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), "")

    def test_skips_when_checkout_is_dirty_or_on_feature_branch_or_paused(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            (state / "off").write_text("")
            module.run = lambda *a, **kw: self.fail("must not run commands when off")
            self.assertFalse(module.deploy_once(repo, state))
            (state / "off").unlink()

            # Feature branch skip
            module.run = lambda args, **kw: subprocess.CompletedProcess(
                args, 0, stdout="feature-branch\n", stderr=""
            )
            self.assertFalse(module.deploy_once(repo, state))

            # Dirty tracked files skip
            def dirty_run(args, **kw):
                if args == ["git", "rev-parse", "--abbrev-ref", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout="main\n", stderr="")
                if args == ["git", "status", "--porcelain", "--untracked-files=no"]:
                    return subprocess.CompletedProcess(args, 0, stdout=" M src/chat.rs\n", stderr="")
                self.fail(f"unexpected command on dirty tree: {args}")

            module.run = dirty_run
            self.assertFalse(module.deploy_once(repo, state))

            # Ahead of origin/main (merge --ff-only is no-op, HEAD stays ahead) must not redeploy
            ahead_sha = "f" * 40
            remote_sha = "e" * 40
            module.write_deployed_sha(state, remote_sha)
            ahead_calls = []

            def ahead_run(args, **kw):
                ahead_calls.append(args)
                if args == ["git", "rev-parse", "--abbrev-ref", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout="main\n", stderr="")
                if args == ["git", "status", "--porcelain", "--untracked-files=no"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "fetch", "--quiet", "origin", "main"]:
                    return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
                if args == ["git", "rev-parse", "HEAD"]:
                    return subprocess.CompletedProcess(args, 0, stdout=f"{ahead_sha}\n", stderr="")
                if args == ["git", "rev-parse", "origin/main"]:
                    return subprocess.CompletedProcess(args, 0, stdout=f"{remote_sha}\n", stderr="")
                if args == ["git", "merge", "--ff-only", "origin/main"]:
                    return subprocess.CompletedProcess(args, 0, stdout="Already up to date.\n", stderr="")
                self.fail(f"unexpected command when ahead of remote: {args}")

            module.run = ahead_run
            self.assertFalse(module.deploy_once(repo, state))
            self.assertFalse(any(cmd[0] == "docker" for cmd in ahead_calls))


if __name__ == "__main__":
    unittest.main()
