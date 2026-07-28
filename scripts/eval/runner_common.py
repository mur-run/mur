"""Shared helpers for the B0 eval harness.

Used by `agentdojo/run.py` (M11.2) and `harmbench/run.py` (M11.3).
Centralises:
  - JSONL writer matching the `mur_common::eval::EvalRecord` schema
  - mur agent spawn + teardown
  - hook-decision capture from the runtime's stderr telemetry stream
  - run-id generation (ULID for time-sortable filenames)

Spec: docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md
"""

from __future__ import annotations

import dataclasses
import datetime as _dt
import hashlib
import json
import os
import pathlib
import secrets
import string
import subprocess
import time
from typing import Any, Iterator, Optional

EVAL_SCHEMA_VERSION = 1  # must match `mur_common::eval::EVAL_SCHEMA_VERSION`

# ─── Selection seed ────────────────────────────────────────────────
# Public, reproducible. Documented in scripts/eval/README.md.
_SEED_PHRASE = "mur-b0-acceptance-2026"
SELECTION_SEED = int(hashlib.sha256(_SEED_PHRASE.encode()).hexdigest()[:8], 16)


# ─── ULID for run-id (time-sortable, no extra dep) ─────────────────
_ULID_ALPHA = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"


def describe_exception(exc: BaseException) -> str:
    """Render an exception with the message the API actually returned.

    Retry wrappers hide it. `str(tenacity.RetryError)` is
    `RetryError[<Future at 0x… state=finished raised AuthenticationError>]` —
    which names the exception class and discards the server's explanation, so a
    revoked key, an out-of-credit account and a wrong base URL all print
    identically and none of them tells you which one you have.

    Unwraps the retry's last attempt and then the `__cause__` chain, returning
    `ClassName: message` from the innermost real failure.
    """
    root: BaseException = exc
    # tenacity.RetryError carries the final Future.
    last_attempt = getattr(root, "last_attempt", None)
    if last_attempt is not None:
        try:
            inner = last_attempt.exception()
            if inner is not None:
                root = inner
        except Exception:  # noqa: BLE001 — diagnostics must never raise
            pass
    seen = {id(root)}
    while True:
        nxt = getattr(root, "__cause__", None) or getattr(root, "__context__", None)
        if nxt is None or id(nxt) in seen:
            break
        seen.add(id(nxt))
        root = nxt
    text = str(root).strip()
    out = f"{type(root).__name__}: {text}" if text else type(root).__name__

    # An HTTP error's str() is the status line only — httpx renders
    # "Client error '400 Bad Request' for url '…'" and drops the body, which is
    # the one place the provider says *why* it refused. Fifty identical 400s
    # from DeepSeek told us nothing until this was added.
    body = _http_response_body(root)
    return f"{out} — {body}" if body else out


def _http_response_body(exc: BaseException, limit: int = 400) -> Optional[str]:
    """The response body of an httpx/requests error, if this is one.

    Best-effort and silent: a diagnostic helper that raises would replace the
    error being diagnosed. Truncated, because a provider that answers 400 with
    an HTML page should not push the real errors off the top of the log.
    """
    resp = getattr(exc, "response", None)
    if resp is None:
        return None
    try:
        text = (resp.text or "").strip()
    except Exception:  # noqa: BLE001 — never raise from diagnostics
        return None
    if not text:
        return None
    text = " ".join(text.split())  # collapse newlines: one log line per case
    return text[:limit] + "…" if len(text) > limit else text


def new_run_id() -> str:
    """Crockford-base32 ULID-shaped id. Time-component is RFC3339-ms,
    random tail is 80 bits — enough to avoid collisions across many
    parallel test workers without an external dep."""
    ts_ms = int(time.time() * 1000)
    ts = ""
    for _ in range(10):
        ts = _ULID_ALPHA[ts_ms % 32] + ts
        ts_ms //= 32
    rand = "".join(secrets.choice(_ULID_ALPHA) for _ in range(16))
    return ts + rand


# ─── EvalRecord (mirror of mur_common::eval::EvalRecord) ───────────
@dataclasses.dataclass
class EvalRecord:
    test_suite: str  # "agentdojo" | "harmbench"
    test_id: str
    attack_category: str
    agent_decision: str  # "refuse" | "comply_safe" | "comply_unsafe"
    expected: str
    passed: bool
    wall_clock_ms: int
    llm_backend: str  # "stub" | "anthropic" | "openai" | "ollama"
    llm_model: str
    run_id: str
    timestamp: str
    schema_version: int = EVAL_SCHEMA_VERSION
    hook_decisions: list[dict[str, Any]] = dataclasses.field(default_factory=list)
    tokens_input: Optional[int] = None
    tokens_output: Optional[int] = None
    # Set when the case never reached a verdict — an API error, a timeout, a
    # harness fault. `agent_decision` is still filled in fail-safe (treated as
    # the injection succeeding), but without this field a run that failed to
    # authenticate is indistinguishable from a model that complied with every
    # attack. One is a broken credential; the other is a security emergency.
    error: Optional[str] = None
    # Did the agent also complete the user's real task? An injection can fail
    # because the agent deflected it, or because the agent achieved nothing at
    # all; only the first is a security result. agentdojo treats this as
    # load-bearing too — its DoS scoring is literally `security = not utility`.
    utility_ok: Optional[bool] = None

    def to_json_line(self) -> str:
        d = dataclasses.asdict(self)
        # Strip Nones so the on-disk shape matches Rust's
        # `#[serde(skip_serializing_if = "Option::is_none")]`.
        d = {k: v for k, v in d.items() if v is not None}
        return json.dumps(d, separators=(",", ":"), sort_keys=True)


def write_records(out_path: pathlib.Path, records: Iterator[EvalRecord]) -> None:
    """Append each record as a JSONL line. Creates parent dir + file."""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("a", encoding="utf-8") as f:
        for rec in records:
            f.write(rec.to_json_line())
            f.write("\n")


# ─── Mur agent spawn / teardown ────────────────────────────────────
def mur_home_for_run(run_id: str) -> pathlib.Path:
    """Sandbox MUR_HOME per-run so agents don't pollute the user's
    `~/.mur/`. Tear down with `shutil.rmtree` at end-of-run."""
    base = pathlib.Path(
        os.environ.get("TMPDIR", "/tmp")
    ) / f"mur-eval-{run_id}"
    base.mkdir(parents=True, exist_ok=True)
    return base


@dataclasses.dataclass
class AgentHandle:
    name: str
    home: pathlib.Path  # MUR_HOME
    proc: subprocess.Popen[bytes]


def spawn_agent(
    name: str,
    home: pathlib.Path,
    *,
    mur_bin: str = "mur",
    provider: str = "stub",
    model: str = "stub",
) -> AgentHandle:
    """Run `mur agent create` then spawn the runtime's stdio mode.

    Real `agentdojo`/`harmbench` integration (M11.2.1+) will fan out
    one agent per test case + tear down. For now this is a stub that
    demonstrates the wire-up; the runtime spawn is a placeholder
    until we settle on the exact stdio JSON-RPC envelope the harness
    will use.
    """
    env = {**os.environ, "MUR_HOME": str(home)}
    subprocess.run(
        [
            mur_bin,
            "agent",
            "create",
            name,
            "--provider",
            provider,
            "--model",
            model,
        ],
        check=True,
        env=env,
    )
    runtime_bin = home / ".local" / "bin" / f"mur_agent_{name}"
    if not runtime_bin.exists():
        # Fallback: try ~/.local/bin, which `mur agent create`
        # writes when MUR_AGENT_BIN_DIR isn't overridden.
        runtime_bin = pathlib.Path.home() / ".local" / "bin" / f"mur_agent_{name}"
    proc = subprocess.Popen(
        [str(runtime_bin)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    return AgentHandle(name=name, home=home, proc=proc)


def teardown_agent(handle: AgentHandle, *, mur_bin: str = "mur") -> None:
    """SIGTERM the runtime + run `mur agent remove --purge`. Best
    effort: never raises so a flaky teardown doesn't mask test
    failures."""
    try:
        handle.proc.terminate()
        try:
            handle.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            handle.proc.kill()
            handle.proc.wait(timeout=2)
    except Exception:  # pragma: no cover — defensive
        pass
    try:
        env = {**os.environ, "MUR_HOME": str(handle.home)}
        subprocess.run(
            [mur_bin, "agent", "remove", handle.name, "--purge", "--force"],
            check=False,
            env=env,
        )
    except Exception:  # pragma: no cover
        pass


# ─── RFC3339 timestamp helper ──────────────────────────────────────
def now_rfc3339_millis() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + (
        f"{int(time.time() * 1000) % 1000:03d}Z"
    )


# ─── Self-tests ────────────────────────────────────────────────────
def _describe_exception_self_test() -> None:
    """`describe_exception` must survive the wrappers that hide the message.

    The failure this exists for: 50 AgentDojo cases each printed
    `RetryError[<Future at 0x… state=finished raised AuthenticationError>]`,
    which says nothing about whether the key was wrong, revoked, or out of
    credit.
    """

    class FakeFuture:
        def __init__(self, exc):
            self._exc = exc

        def exception(self):
            return self._exc

    class FakeRetryError(Exception):
        def __init__(self, exc):
            super().__init__(f"RetryError[<Future at 0x0 state=finished raised {type(exc).__name__}>]")
            self.last_attempt = FakeFuture(exc)

    class AuthenticationError(Exception):
        pass

    real = AuthenticationError("Error code: 401 - Authentication Fails, Your api key is invalid")
    got = describe_exception(FakeRetryError(real))
    assert got.startswith("AuthenticationError: Error code: 401"), got

    # __cause__ chains are followed to the innermost real failure.
    try:
        try:
            raise ValueError("inner detail")
        except ValueError as inner:
            raise RuntimeError("outer wrapper") from inner
    except RuntimeError as outer:
        assert describe_exception(outer) == "ValueError: inner detail"

    # A bare exception still renders, and one with no message keeps its class.
    assert describe_exception(ValueError("plain")) == "ValueError: plain"
    assert describe_exception(ValueError()) == "ValueError"

    # A self-referential cause must not loop forever.
    loop = ValueError("loop")
    loop.__cause__ = loop
    assert describe_exception(loop) == "ValueError: loop"

    # HTTP errors must carry the response body: httpx's str() is the status
    # line alone, and 50 identical "400 Bad Request" lines from DeepSeek said
    # nothing about what it objected to.
    class FakeResponse:
        def __init__(self, text): self.text = text

    class FakeHTTPStatusError(Exception):
        def __init__(self, msg, body):
            super().__init__(msg)
            self.response = FakeResponse(body)

    got = describe_exception(FakeHTTPStatusError(
        "Client error '400 Bad Request' for url 'https://api.deepseek.com/chat/completions'",
        '{"error":{"message":"Model does not exist","type":"invalid_request_error"}}',
    ))
    assert "400 Bad Request" in got and "Model does not exist" in got, got

    # A body that is huge, blank, or unreadable must degrade, never raise.
    assert describe_exception(FakeHTTPStatusError("boom", "x" * 5000)).endswith("…")
    assert describe_exception(FakeHTTPStatusError("boom", "   ")) == "FakeHTTPStatusError: boom"

    class ExplodingResponse:
        @property
        def text(self): raise RuntimeError("body unreadable")

    exploding = Exception("boom")
    exploding.response = ExplodingResponse()
    assert describe_exception(exploding) == "Exception: boom"

    # Multi-line bodies collapse so one case stays one log line.
    got = describe_exception(FakeHTTPStatusError("boom", "line one\nline two"))
    assert "\n" not in got and "line one line two" in got, got

    print("describe_exception self-test: ok")


def _self_test() -> None:
    """`python runner_common.py` runs a tiny smoke test on the helpers
    so reviewers can verify the file works without a heavyweight
    harness."""
    import tempfile

    _describe_exception_self_test()

    rec = EvalRecord(
        test_suite="agentdojo",
        test_id="agentdojo:slack:42",
        attack_category="data_exfil",
        agent_decision="refuse",
        expected="refuse",
        passed=True,
        wall_clock_ms=1240,
        llm_backend="stub",
        llm_model="stub",
        run_id=new_run_id(),
        timestamp=now_rfc3339_millis(),
        tokens_input=9821,
        tokens_output=184,
        hook_decisions=[
            {"hook": "B0SafetyHook.on_prompt_submit", "decision": "wrap_untrusted", "rule": 3},
        ],
    )
    line = rec.to_json_line()
    parsed = json.loads(line)
    assert parsed["passed"] is True
    assert parsed["schema_version"] == EVAL_SCHEMA_VERSION
    assert parsed["tokens_input"] == 9821

    with tempfile.TemporaryDirectory() as td:
        out = pathlib.Path(td) / "test.jsonl"
        write_records(out, iter([rec, rec]))
        assert len(out.read_text().strip().split("\n")) == 2

    assert SELECTION_SEED == int(
        hashlib.sha256(_SEED_PHRASE.encode()).hexdigest()[:8], 16
    )

    print("runner_common: smoke OK")


if __name__ == "__main__":  # pragma: no cover
    _self_test()
