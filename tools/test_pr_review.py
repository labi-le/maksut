"""Tests for the independent pull-request reviewer in `tools/pr_review.py`."""

import io
import json
import unittest
from contextlib import redirect_stderr, redirect_stdout

try:
    from tools import pr_review
except ModuleNotFoundError:
    import pr_review


def pull(**overrides):
    value = {
        "number": 31,
        "title": "chat: stop lowercasing replies at delivery",
        "body": "",
        "draft": False,
        "changed_files": 3,
        "additions": 12,
        "deletions": 30,
        "head": {"sha": "a" * 40},
        "base": {"ref": "main"},
        "user": {"login": "int8-ptr"},
    }
    value.update(overrides)
    return value


CLEAN = {"summary": "Изменение снимает приведение к нижнему регистру.", "verdict": "approve", "findings": []}


class FakeGitHub:
    def __init__(self, *, token="reviewer-token", login="maksut-reviewer", pull_value=None, diff="+hello\n-world\n",
                 reviews=None, fail_submit=False):
        self.token = token
        self.login = login
        self.pull_value = pull_value or pull()
        self.diff_value = diff
        self.reviews_value = [] if reviews is None else reviews
        self.fail_submit = fail_submit
        self.submitted = []

    def pull(self, number):
        return self.pull_value

    def diff(self, number):
        return self.diff_value

    def reviews(self, number):
        return self.reviews_value

    def viewer_login(self):
        return self.login

    def submit_review(self, number, review):
        if self.fail_submit:
            raise pr_review.ReviewError("POST submit: HTTP 422")
        self.submitted.append(review)
        return {"id": 900 + len(self.submitted)}


def reviewer(github, answer=CLEAN, **kwargs):
    replies = answer if isinstance(answer, list) else [answer]
    replies = [json.dumps(item) if isinstance(item, dict) else item for item in replies]

    def complete(prompt):
        return replies.pop(0)

    return pr_review.Reviewer(
        github=github,
        complete=complete,
        repo="labi-le/maxwell",
        number=31,
        model="gemini-4-argon",
        log=lambda message: None,
        **kwargs,
    )


class ParseTests(unittest.TestCase):
    def test_plain_and_fenced_json_are_accepted(self):
        payload = {"summary": "ok", "verdict": "approve", "findings": []}
        self.assertEqual(pr_review.parse_review(json.dumps(payload)), payload)
        fenced = "```json\n" + json.dumps(payload) + "\n```"
        self.assertEqual(pr_review.parse_review(fenced), payload)

    def test_findings_are_normalised(self):
        answer = json.dumps({
            "summary": "s",
            "verdict": "request_changes",
            "findings": [{"severity": "p1", "file": "src/chat.rs", "line": 12, "claim": "c", "evidence": "e"}],
        })
        review = pr_review.parse_review(answer)
        self.assertEqual(review["findings"][0]["severity"], "P1")
        self.assertEqual(review["findings"][0]["line"], 12)

    def test_invalid_severity_missing_claim_and_bad_verdict_are_rejected(self):
        for payload in (
            {"summary": "s", "verdict": "approve", "findings": [{"severity": "P9", "file": "a", "claim": "c"}]},
            {"summary": "s", "verdict": "approve", "findings": [{"severity": "P1", "file": "a", "claim": ""}]},
            {"summary": "s", "verdict": "lgtm", "findings": []},
            {"summary": "s", "verdict": "approve", "findings": "none"},
        ):
            with self.assertRaises(pr_review.ReviewError):
                pr_review.parse_review(json.dumps(payload))

    def test_answer_without_json_object_is_rejected(self):
        with self.assertRaises(pr_review.ReviewError):
            pr_review.parse_review("I think this looks fine")

    def test_non_integer_line_is_dropped(self):
        answer = json.dumps({
            "summary": "s",
            "verdict": "approve",
            "findings": [{"severity": "P3", "file": "a.rs", "line": "12", "claim": "c"}],
        })
        self.assertIsNone(pr_review.parse_review(answer)["findings"][0]["line"])


class EventTests(unittest.TestCase):
    def review(self, *severities, verdict="approve"):
        return {
            "summary": "",
            "verdict": verdict,
            "findings": [
                {"severity": severity, "file": "a.rs", "line": 1, "claim": "c", "evidence": ""}
                for severity in severities
            ],
        }

    def test_blocking_findings_request_changes(self):
        self.assertEqual(pr_review.decide_event(self.review("P0")), "REQUEST_CHANGES")
        self.assertEqual(pr_review.decide_event(self.review("P2", "P1")), "REQUEST_CHANGES")

    def test_non_blocking_findings_approve(self):
        self.assertEqual(pr_review.decide_event(self.review("P2", "P3")), "APPROVE")

    def test_model_verdict_alone_can_request_changes(self):
        self.assertEqual(pr_review.decide_event(self.review(verdict="request_changes")), "REQUEST_CHANGES")


class BodyTests(unittest.TestCase):
    def test_body_lists_findings_with_locations_and_head_sha(self):
        review = {
            "summary": "Кратко.",
            "verdict": "request_changes",
            "findings": [
                {"severity": "P1", "file": "src/b.rs", "line": 7, "claim": "вторая", "evidence": "e2"},
                {"severity": "P0", "file": "src/a.rs", "line": None, "claim": "первая", "evidence": "e1"},
            ],
        }
        body = pr_review.review_body(review, "REQUEST_CHANGES", "b" * 40, "gemini-4-argon")
        self.assertIn(pr_review.REVIEW_MARKER, body)
        self.assertIn("`b" * 1, body)
        self.assertIn("src/a.rs", body)
        self.assertIn("src/b.rs:7", body)
        self.assertLess(body.index("P0 — `src/a.rs`"), body.index("P1 — `src/b.rs:7`"))
        self.assertIn("P0: 1, P1: 1", body)

    def test_body_is_capped(self):
        review = {
            "summary": "s" * 1000,
            "verdict": "approve",
            "findings": [
                {"severity": "P3", "file": "a.rs", "line": 1, "claim": "c" * 2000, "evidence": "e" * 2000}
                for _ in range(40)
            ],
        }
        body = pr_review.review_body(review, "APPROVE", "c" * 40, "gemini-4-argon")
        self.assertLessEqual(len(body), pr_review.MAX_REVIEW_BODY_CHARS + 40)
        self.assertIn("[ревью обрезано", body)


class PromptTests(unittest.TestCase):
    def test_prompt_carries_agents_diff_and_truncation_marker(self):
        prompt = pr_review.build_prompt(pull(body="b" * 10), "+line\n" * 10, "# AGENTS\nrule", "gemini-4-argon")
        self.assertIn("Pull request #31", prompt)
        self.assertIn("# AGENTS.md", prompt)
        self.assertIn("+line", prompt)
        self.assertIn("gemini-4-argon", prompt)

    def test_oversized_diff_is_cut_with_a_marker(self):
        original = pr_review.MAX_DIFF_CHARS
        try:
            pr_review.MAX_DIFF_CHARS = 10
            prompt = pr_review.build_prompt(pull(), "x" * 100, "", "m")
        finally:
            pr_review.MAX_DIFF_CHARS = original
        self.assertIn("[diff обрезан: 90 символов отброшено]", prompt)

    def test_missing_agents_file_is_not_fatal(self):
        self.assertEqual(pr_review.read_text("/nonexistent/AGENTS.md", 10), "")


class RunTests(unittest.TestCase):
    def test_clean_review_is_approved_and_submitted(self):
        github = FakeGitHub()
        result = reviewer(github).run()
        self.assertTrue(result["submitted"])
        self.assertEqual(github.submitted[0]["event"], "APPROVE")
        self.assertEqual(github.submitted[0]["commit_id"], "a" * 40)

    def test_blocking_finding_requests_changes(self):
        github = FakeGitHub()
        answer = json.dumps({
            "summary": "s",
            "verdict": "approve",
            "findings": [{"severity": "P1", "file": "src/chat.rs", "line": 3, "claim": "c", "evidence": "e"}],
        })
        result = reviewer(github, answer=answer).run()
        self.assertEqual(result["event"], "REQUEST_CHANGES")
        self.assertEqual(github.submitted[0]["event"], "REQUEST_CHANGES")

    def test_missing_token_skips_without_any_api_call(self):
        github = FakeGitHub(token="")
        self.assertFalse(reviewer(github).run()["submitted"])
        self.assertEqual(github.submitted, [])

    def test_draft_and_self_authored_and_already_reviewed_are_skipped(self):
        draft = reviewer(FakeGitHub(pull_value=pull(draft=True)))
        self.assertIn("draft", draft.run()["skipped"])
        own = reviewer(FakeGitHub(login="int8-ptr"))
        self.assertIn("author", own.run()["skipped"])
        seen = reviewer(FakeGitHub(reviews=[{"user": {"login": "Maksut-Reviewer"}, "commit_id": "a" * 40}]))
        self.assertIn("already reviewed", seen.run()["skipped"])

    def test_review_at_another_head_sha_does_not_skip(self):
        github = FakeGitHub(reviews=[{"user": {"login": "maksut-reviewer"}, "commit_id": "b" * 40}])
        self.assertTrue(reviewer(github).run()["submitted"])

    def test_empty_diff_is_skipped(self):
        self.assertIn("empty diff", reviewer(FakeGitHub(diff=" \n")).run()["skipped"])

    def test_invalid_model_answer_is_retried_once_then_fails(self):
        github = FakeGitHub()
        instance = reviewer(github, answer=["not json", "still not json"])
        with self.assertRaises(pr_review.ReviewError):
            instance.run()
        self.assertEqual(github.submitted, [])

    def test_retry_can_recover(self):
        github = FakeGitHub()
        answer = json.dumps({"summary": "s", "verdict": "approve", "findings": []})
        instance = reviewer(github, answer=["nope", answer])
        self.assertTrue(instance.run()["submitted"])

    def test_dry_run_does_not_submit(self):
        github = FakeGitHub()
        with redirect_stdout(io.StringIO()) as printed:
            result = reviewer(github, dry_run=True).run()
        self.assertTrue(result["dry_run"])
        self.assertEqual(github.submitted, [])
        self.assertIn('"event": "APPROVE"', printed.getvalue())


class MainTests(unittest.TestCase):
    def test_missing_repository_or_number_fails(self):
        with redirect_stderr(io.StringIO()):
            self.assertEqual(pr_review.main({}), 1)
            self.assertEqual(pr_review.main({"GITHUB_REPOSITORY": "labi-le/maxwell"}), 1)

    def test_missing_token_exits_zero_without_network(self):
        with redirect_stderr(io.StringIO()) as log:
            code = pr_review.main({"GITHUB_REPOSITORY": "labi-le/maxwell", "PR_NUMBER": "31"})
        self.assertEqual(code, 0)
        self.assertIn("GH_TOKEN is empty", log.getvalue())

    def test_missing_api_key_fails(self):
        with redirect_stderr(io.StringIO()):
            self.assertEqual(
                pr_review.main({"GITHUB_REPOSITORY": "labi-le/maxwell", "PR_NUMBER": "31", "GH_TOKEN": "t"}),
                1,
            )


if __name__ == "__main__":
    unittest.main()
