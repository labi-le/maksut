import importlib.util
import io
import json
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

IMAGE = "ghcr.io/labi-le/maksut"
# Pinned literally so a narrowed export list cannot silently drop a mounted path.
EXPORTED_PATHS = ("src", "tools", "docs", "Cargo.toml", "README.md", "AGENTS.md")


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
    os.environ["AUTO_DEPLOY_IMAGE_REPO"] = IMAGE
    spec = importlib.util.spec_from_file_location(
        "auto_deploy", pathlib.Path(__file__).with_name("auto_deploy.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, repo, state


def tar_with(files: dict[str, str]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, text in files.items():
            raw = text.encode()
            info = tarfile.TarInfo(name)
            info.size = len(raw)
            archive.addfile(info, io.BytesIO(raw))
    return buffer.getvalue()


def container_state(
    image: str, code_dir: str, running: bool = True, status: str | None = None
) -> dict:
    """The `{running, status, image, mounts}` object `docker inspect --format` emits."""
    source = code_dir if code_dir.endswith("/src") else f"{code_dir}/src"
    return {
        "running": running,
        "status": status or ("running" if running else "exited"),
        "image": image,
        "mounts": [
            {"Type": "bind", "Source": source, "Destination": "/var/lib/maksut/code/src"},
        ],
    }


class FakeGitAndDocker:
    """Answers the exact command shapes `auto_deploy.py` issues."""

    def __init__(
        self,
        *,
        branch: str = "main",
        dirty: str = "",
        head_sha: str = "a" * 40,
        remote_sha: str = "b" * 40,
        pull_ok: bool = True,
        pull_stderr: str = "",
        merge_ok: bool = True,
        compose_in_commit: bool = True,
        archive_ok: bool = True,
        up_ok: bool = True,
        container: dict | None = None,
        qdrant_container: dict | None = None,
        embeddings_container: dict | None = None,
    ):
        self.branch = branch
        self.dirty = dirty
        self.head_sha = head_sha
        self.remote_sha = remote_sha
        self.pull_ok = pull_ok
        self.pull_stderr = pull_stderr
        self.merge_ok = merge_ok
        self.compose_in_commit = compose_in_commit
        self.archive_ok = archive_ok
        self.up_ok = up_ok
        self.container = container
        self.qdrant_container = qdrant_container
        self.embeddings_container = embeddings_container
        self.calls: list[list[str]] = []
        self.envs: list[dict[str, str]] = []

    def run(self, args, timeout=60, cwd=None, env=None):
        self.calls.append(list(args))
        if env is not None:
            self.envs.append(dict(env))
        if args == ["git", "fetch", "--quiet", "origin", "main"]:
            return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
        if args == ["git", "rev-parse", "origin/main"]:
            return subprocess.CompletedProcess(args, 0, stdout=f"{self.remote_sha}\n", stderr="")
        if args == ["git", "rev-parse", "--abbrev-ref", "HEAD"]:
            return subprocess.CompletedProcess(args, 0, stdout=f"{self.branch}\n", stderr="")
        if args == ["git", "status", "--porcelain", "--untracked-files=no"]:
            return subprocess.CompletedProcess(args, 0, stdout=self.dirty, stderr="")
        if args == ["git", "rev-parse", "HEAD"]:
            return subprocess.CompletedProcess(args, 0, stdout=f"{self.head_sha}\n", stderr="")
        if args == ["git", "merge", "--ff-only", "origin/main"]:
            if not self.merge_ok:
                return subprocess.CompletedProcess(args, 1, stdout="", stderr="merge refused")
            self.head_sha = self.remote_sha
            return subprocess.CompletedProcess(args, 0, stdout="Fast-forward\n", stderr="")
        if args[:2] == ["docker", "inspect"]:
            name = args[-1]
            source = self.container
            if name == "maksut-qdrant-1":
                source = self.qdrant_container if self.qdrant_container is not None else self.container
            elif name == "maksut-embeddings-1":
                source = (
                    self.embeddings_container
                    if self.embeddings_container is not None
                    else self.container
                )
            if source is None:
                return subprocess.CompletedProcess(
                    args, 1, stdout="", stderr=f"Error: No such object: {name}"
                )
            # Plain `docker inspect` prints a list of documents; the deploy only
            # parses the single object its `--format` template emits.
            payload = source if args[2:3] == ["--format"] else [source]
            return subprocess.CompletedProcess(args, 0, stdout=json.dumps(payload), stderr="")
        if args[:3] == ["docker", "pull", "--quiet"]:
            code = 0 if self.pull_ok else 1
            return subprocess.CompletedProcess(args, code, stdout="", stderr=self.pull_stderr)
        if args[:3] == ["docker", "compose", "-f"]:
            code = 0 if self.up_ok else 1
            return subprocess.CompletedProcess(args, code, stdout="Started\n", stderr="")
        if args[:3] == ["docker", "image", "rm"]:
            return subprocess.CompletedProcess(args, 0, stdout="Untagged\n", stderr="")
        raise AssertionError(f"unexpected command: {args}")

    def run_bytes(self, args, timeout=60, cwd=None):
        self.calls.append(list(args))
        if args[:3] == ["git", "archive", "--format=tar"]:
            if not self.archive_ok:
                return subprocess.CompletedProcess(args, 128, stdout=b"", stderr=b"pathspec did not match")
            payload = tar_with({"src/main.rs": "fn main() {}\n", "docs/guide.md": "# guide\n"})
            return subprocess.CompletedProcess(args, 0, stdout=payload, stderr=b"")
        if args[0] == "git" and args[1] == "show":
            if not self.compose_in_commit:
                return subprocess.CompletedProcess(args, 1, stdout=b"", stderr=b"missing")
            return subprocess.CompletedProcess(
                args, 0, stdout=b"name: maksut\nservices:\n  maksut:\n    image: x\n", stderr=b""
            )
        raise AssertionError(f"unexpected byte command: {args}")

    def named(self, prefix):
        return [call for call in self.calls if call[: len(prefix)] == prefix]


def install(module, fake):
    module.run = fake.run
    module.run_bytes = fake.run_bytes
    return fake


class AutoDeployTests(unittest.TestCase):
    def test_dirty_checkout_still_deploys_the_remote_commit(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            fake = install(
                module,
                FakeGitAndDocker(dirty=" M src/chat.rs\n", head_sha="a" * 40, remote_sha="b" * 40),
            )
            self.assertTrue(module.deploy_once(repo, state))
            self.assertEqual(fake.named(["git", "merge"]), [])
            self.assertEqual(module.read_deployed_sha(state), "b" * 40)
            export = state / "code" / ("b" * 40)
            self.assertEqual((export / "src" / "main.rs").read_text(), "fn main() {}\n")
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            self.assertIn("--no-build", ups[0])
            self.assertIn("qdrant", ups[0])
            self.assertIn("embeddings", ups[0])
            self.assertTrue(ups[0][3].endswith(module.COMPOSE_OVERRIDE))
            env = [item for item in fake.envs if item.get("MAKSUT_IMAGE_TAG")][0]
            self.assertEqual(env["MAKSUT_IMAGE_TAG"], "b" * 40)
            self.assertEqual(env["MAKSUT_CODE_DIR"], str(export))
            self.assertFalse((repo / module.COMPOSE_OVERRIDE).exists())
            self.assertIn(
                ["git", "archive", "--format=tar", "b" * 40, "--", *EXPORTED_PATHS],
                fake.calls,
            )
            self.assertIn(["git", "show", f"{'b' * 40}:compose.yaml"], fake.calls)

    def test_clean_checkout_is_fast_forwarded_and_the_old_state_is_pruned(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            old_sha = "a" * 40
            module.write_deployed_sha(state, old_sha)
            stale = state / "code" / old_sha
            stale.mkdir(parents=True)
            (stale / "src").mkdir()
            fake = install(module, FakeGitAndDocker(head_sha=old_sha, remote_sha="b" * 40))
            self.assertTrue(module.deploy_once(repo, state))
            merge_index = fake.calls.index(["git", "merge", "--ff-only", "origin/main"])
            up_index = next(
                index for index, call in enumerate(fake.calls) if call[:3] == ["docker", "compose", "-f"]
            )
            self.assertLess(merge_index, up_index)
            self.assertIn(["docker", "image", "rm", f"{IMAGE}:{old_sha}"], fake.calls)
            self.assertFalse(stale.exists())
            self.assertIn(["git", "archive", "--format=tar", "b" * 40, "--", *EXPORTED_PATHS], fake.calls)

    def test_unpublished_image_waits_without_touching_the_container(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            fake = install(
                module, FakeGitAndDocker(pull_ok=False, pull_stderr="manifest unknown")
            )
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), "")
            self.assertEqual(fake.named(["docker", "compose"]), [])
            self.assertFalse((state / "code").exists())

    def test_same_sha_does_not_pull_or_recreate(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", str(state / "code" / sha)),
                ),
            )
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(fake.named(["docker", "pull"]), [])
            self.assertEqual(fake.named(["docker", "compose"]), [])
            self.assertEqual(fake.named(["git", "merge"]), [])

    def test_idle_tick_catches_a_behind_checkout_up(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha="a" * 40,
                    container=container_state(f"{IMAGE}:{sha}", str(state / "code" / sha)),
                ),
            )
            self.assertFalse(module.deploy_once(repo, state))
            self.assertIn(["git", "merge", "--ff-only", "origin/main"], fake.calls)
            self.assertEqual(fake.named(["docker", "compose"]), [])

    def test_same_sha_with_a_foreign_image_recreates_the_container(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{'c' * 40}", str(state / "code" / sha)),
                ),
            )
            with mock.patch.object(module, "log") as logged, mock.patch.object(
                module, "write_deployed_sha"
            ) as writer:
                self.assertTrue(module.deploy_once(repo, state))
            writer.assert_not_called()
            reasons = [call.args[0] for call in logged.call_args_list]
            self.assertTrue(
                any(reason.startswith("reconciling") and "!=" in reason for reason in reasons),
                reasons,
            )
            self.assertIn(["docker", "pull", "--quiet", f"{IMAGE}:{sha}"], fake.calls)
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            env = [item for item in fake.envs if item.get("MAKSUT_IMAGE_TAG")][0]
            self.assertEqual(env["MAKSUT_IMAGE_TAG"], sha)
            self.assertEqual(module.read_deployed_sha(state), sha)

    def test_missing_container_is_recreated_when_the_marker_matches(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(module, FakeGitAndDocker(remote_sha=sha, head_sha=sha))
            self.assertTrue(module.deploy_once(repo, state))
            # The maksut check short-circuits before qdrant is inspected.
            inspect = fake.named(["docker", "inspect"])
            self.assertEqual(len(inspect), 1)
            self.assertEqual(inspect[0][-1], "maksut-maksut-1")
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            env = [item for item in fake.envs if item.get("MAKSUT_IMAGE_TAG")][0]
            self.assertEqual(env["MAKSUT_IMAGE_TAG"], sha)

    def test_foreign_code_mount_is_reconciled(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", str(repo)),
                ),
            )
            self.assertTrue(module.deploy_once(repo, state))
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            env = [item for item in fake.envs if item.get("MAKSUT_CODE_DIR")][0]
            self.assertEqual(env["MAKSUT_CODE_DIR"], str(state / "code" / sha))

    def test_stopped_container_is_recreated_when_the_marker_matches(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(
                        f"{IMAGE}:{sha}", str(state / "code" / sha), running=False
                    ),
                ),
            )
            self.assertTrue(module.deploy_once(repo, state))
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            env = [item for item in fake.envs if item.get("MAKSUT_IMAGE_TAG")][0]
            self.assertEqual(env["MAKSUT_IMAGE_TAG"], sha)

    def test_status_prints_the_container_image_comparison(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", str(state / "code" / sha)),
                ),
            )
            with mock.patch("builtins.print") as printed:
                self.assertEqual(module.status(state), 0)
            self.assertEqual(printed.call_args.args[0], f"ok: running {IMAGE}:{sha}")

            install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{'c' * 40}", str(state / "code" / sha)),
                ),
            )
            with mock.patch("builtins.print") as drifted:
                self.assertEqual(module.status(state), 0)
            self.assertEqual(
                drifted.call_args.args[0], f"container image {IMAGE}:{'c' * 40} != {IMAGE}:{sha}"
            )

    def test_status_argument_stays_read_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", str(state / "code" / sha)),
                ),
            )
            with mock.patch("builtins.print"):
                self.assertEqual(module.main(["--status"]), 0)
            self.assertEqual(fake.named(["docker", "pull"]), [])
            self.assertEqual(fake.named(["docker", "compose"]), [])
            self.assertEqual(fake.named(["git", "fetch"]), [])

    def test_feature_branch_checkout_is_left_alone_but_the_deploy_proceeds(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            fake = install(module, FakeGitAndDocker(branch="wip"))
            self.assertTrue(module.deploy_once(repo, state))
            self.assertEqual(fake.named(["git", "merge"]), [])
            self.assertEqual(len(fake.named(["docker", "compose"])), 1)

    def test_missing_code_export_aborts_the_deploy(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            fake = install(module, FakeGitAndDocker(archive_ok=False))
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), "")
            self.assertEqual(fake.named(["docker", "compose"]), [])

    def test_missing_compose_in_the_commit_aborts_the_deploy(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            fake = install(module, FakeGitAndDocker(compose_in_commit=False))
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), "")
            self.assertEqual(fake.named(["docker", "compose"]), [])

    def test_failed_compose_up_keeps_the_marker_empty(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            module.write_deployed_sha(state, "a" * 40)
            fake = install(module, FakeGitAndDocker(up_ok=False))
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), "a" * 40)
            self.assertEqual(fake.named(["docker", "image", "rm"]), [])
            self.assertFalse((repo / module.COMPOSE_OVERRIDE).exists())

    def test_failed_fast_forward_still_deploys(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            fake = install(module, FakeGitAndDocker(merge_ok=False))
            self.assertTrue(module.deploy_once(repo, state))
            self.assertEqual(module.read_deployed_sha(state), "b" * 40)

    def test_kill_switch_skips_every_command(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            (state / "off").write_text("")
            fake = install(module, FakeGitAndDocker())
            self.assertFalse(module.deploy_once(repo, state))
            self.assertEqual(fake.calls, [])


    def test_stopped_qdrant_container_recreates_the_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            code_dir = str(state / "code" / sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", code_dir),
                    qdrant_container=container_state("qdrant/qdrant:v1.20.0", code_dir, running=False),
                ),
            )
            with mock.patch.object(module, "log") as logged:
                self.assertTrue(module.deploy_once(repo, state))
            reasons = [call.args[0] for call in logged.call_args_list]
            self.assertTrue(
                any("maksut-qdrant-1 is not running" in reason for reason in reasons),
                reasons,
            )
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            self.assertIn("qdrant", ups[0])

    def test_restarting_qdrant_container_recreates_the_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            code_dir = str(state / "code" / sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", code_dir),
                    qdrant_container=container_state(
                        "qdrant/qdrant:v1.20.0", code_dir, running=True, status="restarting"
                    ),
                ),
            )
            with mock.patch.object(module, "log") as logged:
                self.assertTrue(module.deploy_once(repo, state))
            reasons = [call.args[0] for call in logged.call_args_list]
            self.assertTrue(
                any("maksut-qdrant-1 is not running" in reason for reason in reasons),
                reasons,
            )
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            self.assertIn("qdrant", ups[0])

    def test_stopped_embeddings_container_recreates_the_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            module, repo, state = load_module(tmp)
            sha = "b" * 40
            module.write_deployed_sha(state, sha)
            code_dir = str(state / "code" / sha)
            fake = install(
                module,
                FakeGitAndDocker(
                    remote_sha=sha,
                    head_sha=sha,
                    container=container_state(f"{IMAGE}:{sha}", code_dir),
                    embeddings_container=container_state(
                        "ghcr.io/huggingface/text-embeddings-inference:cpu-1.8.3",
                        code_dir,
                        running=False,
                    ),
                ),
            )
            with mock.patch.object(module, "log") as logged:
                self.assertTrue(module.deploy_once(repo, state))
            reasons = [call.args[0] for call in logged.call_args_list]
            self.assertTrue(
                any("maksut-embeddings-1 is not running" in reason for reason in reasons),
                reasons,
            )
            ups = fake.named(["docker", "compose"])
            self.assertEqual(len(ups), 1)
            self.assertIn("embeddings", ups[0])

if __name__ == "__main__":
    unittest.main()
