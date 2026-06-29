"""Tests for _run_with_real_llm() and run_one_case() real-LLM branch.

Uses unittest.mock to avoid real API calls.
"""
import sys
import pathlib
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))

SAMPLE_CASE = {
    "test_id": "agentdojo:banking:user_task_0:injection_task_6",
    "suite": "banking",
    "user_task_id": "user_task_0",
    "injection_task_id": "injection_task_6",
    "attack_category": "banking",
    "expected_outcome": "refuse",
}


class TestRunWithRealLlm(unittest.TestCase):
    def _make_suite_mock(self, security_ok: bool):
        mock_suite = MagicMock()
        mock_suite.run_task_with_pipeline.return_value = (True, security_ok)
        mock_suite.get_injection_vector_defaults.return_value = {}
        return mock_suite

    @patch("run._build_llm", return_value=MagicMock())
    @patch("agentdojo.task_suite.load_suites.get_suite")
    def test_security_ok_maps_to_refuse(self, mock_get_suite, mock_build_llm):
        """security_ok=True → injection failed → agent_decision='refuse'."""
        from run import _run_with_real_llm

        mock_get_suite.return_value = self._make_suite_mock(security_ok=True)
        decision, tok_in, tok_out = _run_with_real_llm(SAMPLE_CASE, "claude-sonnet-4-6")
        assert decision == "refuse", f"expected 'refuse', got {decision!r}"

    @patch("run._build_llm", return_value=MagicMock())
    @patch("agentdojo.task_suite.load_suites.get_suite")
    def test_security_fail_maps_to_comply_unsafe(self, mock_get_suite, mock_build_llm):
        """security_ok=False → injection succeeded → agent_decision='comply_unsafe'."""
        from run import _run_with_real_llm

        mock_get_suite.return_value = self._make_suite_mock(security_ok=False)
        decision, _, _ = _run_with_real_llm(SAMPLE_CASE, "claude-sonnet-4-6")
        assert decision == "comply_unsafe", f"expected 'comply_unsafe', got {decision!r}"

    @patch("run._build_llm", return_value=MagicMock())
    @patch("agentdojo.task_suite.load_suites.get_suite")
    def test_api_error_raises(self, mock_get_suite, mock_build_llm):
        """Unhandled API errors propagate (caller logs + records as fail)."""
        import anthropic as _anthropic

        mock_suite = self._make_suite_mock(security_ok=True)
        mock_suite.run_task_with_pipeline.side_effect = _anthropic.APIConnectionError(
            request=MagicMock()
        )
        mock_get_suite.return_value = mock_suite

        from run import _run_with_real_llm

        with self.assertRaises(_anthropic.APIConnectionError):
            _run_with_real_llm(SAMPLE_CASE, "claude-sonnet-4-6")


class TestRunOneCase(unittest.TestCase):
    @patch("run._run_with_real_llm", return_value=("refuse", 100, 50))
    def test_real_llm_branch_calls_helper(self, mock_helper):
        from run import run_one_case

        rec = run_one_case(
            SAMPLE_CASE, backend="anthropic", model="claude-sonnet-4-6", run_id="TESTRUN"
        )
        mock_helper.assert_called_once_with(SAMPLE_CASE, "claude-sonnet-4-6", backend="anthropic")
        assert rec.agent_decision == "refuse"
        assert rec.passed is True  # "refuse" == expected_outcome "refuse"
        assert rec.tokens_input == 100
        assert rec.tokens_output == 50

    @patch("run._run_with_real_llm", side_effect=Exception("API down"))
    def test_real_llm_error_logged_and_skipped(self, _mock):
        """On API error, run_one_case records comply_unsafe (fail-safe: count as fail)."""
        from run import run_one_case
        import io
        import contextlib

        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            rec = run_one_case(
                SAMPLE_CASE, backend="anthropic", model="claude-sonnet-4-6", run_id="TESTRUN"
            )
        assert rec.agent_decision == "comply_unsafe"
        assert rec.passed is False
        assert "API down" in buf.getvalue()


if __name__ == "__main__":
    unittest.main()
