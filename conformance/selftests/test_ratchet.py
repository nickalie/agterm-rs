import importlib.util
import json
import subprocess
import sys
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest

TOOLS = Path(__file__).resolve().parent.parent / "tools"
TOOL = TOOLS / "check_ratchet.py"
A = "tests/protocol/test_a.py::test_one"
B = "tests/protocol/test_b.py::test_two"
C = "tests/protocol/test_c.py::test_three"
THREE_SPACES = "tests/protocol/test_x.py::test_rename_requires_a_name[   ]"
TWO_SPACES = "tests/protocol/test_x.py::test_rename_requires_a_name[  ]"
HEADER = "# the ratchet\n# sorted ids\n"


def _load(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # dataclasses resolve their module by name
    spec.loader.exec_module(module)
    return module


cr = _load("check_ratchet")


def rec(nodeid: str, outcome: str = "passed", skip: str | None = None, reason: str | None = None) -> dict[str, Any]:
    return {"kind": "test", "nodeid": nodeid, "outcome": outcome, "skip": skip, "reason": reason}


def write_report(path: Path, *records: dict[str, Any], partial: bool = False) -> Path:
    lines = [{"kind": "run", "partial": partial}, *records]
    path.write_text("".join(json.dumps(line) + "\n" for line in lines), encoding="utf-8")
    return path


def write_list(path: Path, *ids: str, header: str = HEADER) -> Path:
    path.write_text(header + "".join(f"{nodeid}\n" for nodeid in ids), encoding="utf-8")
    return path


@pytest.fixture
def files(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> tuple[Path, Path]:
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    return tmp_path / "report.jsonl", tmp_path / "rust-passing.txt"


def check(capsys: pytest.CaptureFixture[str], report: Path, ratchet: Path, *extra: str) -> tuple[int, list[str], str]:
    code = cr.main(["--report", str(report), "--list", str(ratchet), *extra])
    out, err = capsys.readouterr()
    return code, out.splitlines(), err


def test_holding_run(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(A), rec(B, "xpassed", reason="was flaky"))
    write_list(ratchet, A, B)
    code, out, err = check(capsys, report, ratchet)
    assert (code, err) == (0, "")
    assert out == ["ratchet holds: 2 listed id(s), full run"]


@pytest.mark.parametrize(
    ("record", "expected"),
    [
        (rec(A, "failed"), f"BROKEN failed: {A}"),
        (rec(A, "error"), f"BROKEN error: {A}"),
        (
            rec(A, "skipped", "not-yet-at-level", "not-yet-at-level: since 3, rust is at 1"),
            f"BROKEN skipped (not-yet-at-level: since 3, rust is at 1): {A}",
        ),
        (rec(A, "skipped", "other", "needs a mouse"), f"BROKEN skipped (needs a mouse): {A}"),
        (rec(A, "xfailed", reason="known bug"), f"BROKEN xfailed: {A}"),
        (rec(A, "not-run"), f"BROKEN not-run: {A}"),
        (
            rec(A, "skipped", "unsupported", "unsupported: rust lacks posix-shell"),
            f"BROKEN skipped (unsupported: rust lacks posix-shell): {A}",
        ),
        (
            rec(A, "skipped", "unsupported", "unsupported: rust has no ui driver"),
            f"BROKEN skipped (unsupported: rust has no ui driver): {A}",
        ),
    ],
    ids=[
        "failed",
        "error",
        "not-yet-at-level",
        "other-skip",
        "xfailed",
        "not-run",
        "unsupported-posix-shell",
        "unsupported-no-driver",
    ],
)
def test_listed_id_that_does_not_pass_breaks(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str], record: dict[str, Any], expected: str
) -> None:
    report, ratchet = files
    write_report(report, record, rec(B))
    write_list(ratchet, A, B)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 1
    assert out == [expected, "ratchet broken: 1 break(s) over 2 listed id(s)"]


def test_teardown_error_after_a_passing_call_breaks(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]
) -> None:
    """The report merges it into one `error` record; a module fixture's teardown lands on the module's last test."""
    report, ratchet = files
    write_report(report, rec(A), rec(B, "error"))
    write_list(ratchet, A, B)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 1
    assert out == [f"BROKEN error: {B}", "ratchet broken: 1 break(s) over 2 listed id(s)"]


@pytest.mark.parametrize(
    "reason",
    [
        "unsupported: rust lacks live-sessions",
        "unsupported: rust lacks live-sessions on this host: zmx is not installed",
        "unsupported: rust lacks live-any-shell (restores any shell)",
    ],
)
def test_host_dependent_unsupported_skip_is_not_run(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str], reason: str
) -> None:
    report, ratchet = files
    write_report(report, rec(A, "skipped", "unsupported", reason), rec(B))
    write_list(ratchet, A, B)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 0
    assert out == [
        f"not run on this host: skipped ({reason}): {A}",
        "ratchet holds: 2 listed id(s) (1 not run on this host), full run",
    ]


def test_host_dependent_capability_needs_the_unsupported_category() -> None:
    reason = "unsupported: rust lacks live-sessions"
    assert cr.host_dependent_skip(cr.TestRecord(A, "skipped", "unsupported", reason))
    assert not cr.host_dependent_skip(cr.TestRecord(A, "skipped", "other", reason))
    assert not cr.host_dependent_skip(cr.TestRecord(A, "skipped", "unsupported", "unsupported: rust lacks live-x"))


def test_collection_error_breaks(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    message = "Traceback ...\n  line 1\nE   ImportError: no module named zmx\n"
    write_report(report, rec(A), {"kind": "collection-error", "nodeid": "tests/test_d.py", "message": message})
    write_list(ratchet, A)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 1
    assert out[0] == "BROKEN collection error: tests/test_d.py: E   ImportError: no module named zmx"


def test_must_pass_problem_breaks(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(A), {"kind": "must-pass", "problem": f"{B} did not pass"})
    write_list(ratchet, A)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 1
    assert out == [f"BROKEN must-pass: {B} did not pass", "ratchet broken: 1 break(s) over 1 listed id(s)"]


def test_not_collected_on_a_partial_run_is_ignored(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]
) -> None:
    report, ratchet = files
    write_report(report, rec(A), partial=True)
    write_list(ratchet, A, B)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 0
    assert out == ["ratchet holds: 2 listed id(s), partial run"]


def test_not_collected_on_a_full_run_warns(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(A))
    write_list(ratchet, A, B)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 0
    assert out == [
        f"warning: listed but not collected: {B}",
        "ratchet holds: 2 listed id(s) (1 not collected), full run",
    ]


def test_ids_with_spaces_match_verbatim(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(TWO_SPACES), rec(THREE_SPACES, "failed"))
    write_list(ratchet, THREE_SPACES)
    assert cr.read_list(ratchet).ids == [THREE_SPACES]
    code, out, _ = check(capsys, report, ratchet)
    assert code == 1
    assert out == [
        f"BROKEN failed: {THREE_SPACES}",
        "newly passing, not in the list (1):",
        TWO_SPACES,
        "ratchet broken: 1 break(s) over 1 listed id(s)",
    ]


def test_newly_passing_ids_are_printed_and_summarized(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    report, ratchet = files
    summary = tmp_path / "summary.md"
    summary.write_text("earlier step\n")
    monkeypatch.setenv("GITHUB_STEP_SUMMARY", str(summary))
    write_report(report, rec(C), rec(A), rec(B, "xpassed"), rec(THREE_SPACES, "failed"))
    write_list(ratchet, A)
    code, out, _ = check(capsys, report, ratchet)
    assert code == 0
    assert out == [
        "newly passing, not in the list (2):",
        B,
        C,
        "ratchet holds: 1 listed id(s), full run",
    ]
    text = summary.read_text()
    assert text.startswith("earlier step\n### Conformance ratchet\n")
    assert "**ratchet holds: 1 listed id(s), full run**" in text
    assert f"```text\n{B}\n{C}\n```" in text


def test_summary_lists_the_breaks(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    report, ratchet = files
    summary = tmp_path / "summary.md"
    monkeypatch.setenv("GITHUB_STEP_SUMMARY", str(summary))
    write_report(report, rec(A, "failed"))
    write_list(ratchet, A)
    assert check(capsys, report, ratchet)[0] == 1
    assert f"Breaks:\n\n```text\nfailed: {A}\n```" in summary.read_text()


def test_add_merges_sorted_and_keeps_the_header(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(A), rec(C), rec(B), rec(THREE_SPACES), rec(TWO_SPACES, "failed"))
    write_list(ratchet, B)
    code, out, _ = check(capsys, report, ratchet, "--add")
    assert code == 0
    assert out[-1] == f"added 3 id(s) to {ratchet}"
    assert ratchet.read_text() == HEADER + f"{A}\n{B}\n{C}\n{THREE_SPACES}\n"
    assert check(capsys, report, ratchet)[1] == ["ratchet holds: 4 listed id(s), full run"]


def test_add_into_a_header_only_list(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(A))
    write_list(ratchet)
    assert check(capsys, report, ratchet, "--add")[0] == 0
    assert ratchet.read_text() == HEADER + f"{A}\n"


def test_add_is_skipped_when_broken(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec(A, "failed"), rec(B))
    write_list(ratchet, A)
    before = ratchet.read_bytes()
    code, out, err = check(capsys, report, ratchet, "--add")
    assert code == 1
    assert "--add skipped: the ratchet is broken" in err
    assert B in out
    assert ratchet.read_bytes() == before


def test_add_refuses_an_id_the_list_cannot_hold(files: tuple[Path, Path], capsys: pytest.CaptureFixture[str]) -> None:
    report, ratchet = files
    write_report(report, rec("tests/test_e.py::test_bad[a\rb]"))
    write_list(ratchet)
    before = ratchet.read_bytes()
    code, _, err = check(capsys, report, ratchet, "--add")
    assert code == 2
    assert "--add refused" in err and "carriage return" in err
    assert ratchet.read_bytes() == before


@pytest.mark.parametrize(
    ("content", "message"),
    [
        (b"# h\n\xff\xfe\n", "cannot read the ratchet list"),
        (f"# h\n\n{A}\n".encode(), ":2: an empty line"),
        (f"{A}\n\n".encode(), ":2: an empty line"),
        (f"# h\r\n{A}\n".encode(), ":1: a carriage return"),
        (f"{A}\r\n".encode(), ":1: a carriage return"),
        (f"{A}\ntests/protocol/test_b.py\n".encode(), ":2: not a pytest node id"),
        (f"{A}\n{A}\n".encode(), ":2: a duplicate id"),
        (f"{B}\n{A}\n".encode(), ":2: out of order"),
        (f"{TWO_SPACES}\n{THREE_SPACES}\n".encode(), ":2: out of order"),
        (f"# h\n{A}\n# late\n".encode(), ":3: a comment after the first id"),
    ],
    ids=[
        "not-utf8",
        "empty-line",
        "two-trailing-newlines",
        "crlf-header",
        "crlf-id",
        "no-colons",
        "duplicate",
        "out-of-order",
        "spaces-out-of-order",
        "late-comment",
    ],
)
def test_malformed_list(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str], content: bytes, message: str
) -> None:
    report, ratchet = files
    write_report(report, rec(A))
    ratchet.write_bytes(content)
    code, out, err = check(capsys, report, ratchet)
    assert (code, out) == (2, [])
    assert err.startswith(f"check_ratchet: {ratchet}")
    assert message in err


def test_list_without_a_final_newline_is_fine(tmp_path: Path) -> None:
    assert cr.parse_list(f"# h\n{A}\n{B}", tmp_path).ids == [A, B]
    assert cr.parse_list("", tmp_path).ids == []


@pytest.mark.parametrize(
    ("lines", "message"),
    [
        (None, "cannot read the report"),
        ([], "no run header"),
        (['{"kind": "run", "partial": false}', "not json"], ":2: not JSON"),
        (['{"kind": "run", "partial": false}', "[1, 2]"], ":2: not a JSON object"),
        (['{"kind": "run", "partial": false}', ""], ":2: not JSON"),
        ([json.dumps(rec(A))], ":1: a 'test' record before the run header"),
        (['{"kind": "run", "partial": false}', '{"kind": "run", "partial": true}'], ":2: a second run header"),
        (['{"kind": "run", "partial": "no"}'], ":1: 'partial' is missing"),
        (['{"kind": "run", "partial": false}', '{"kind": "summary"}'], ":2: unknown kind 'summary'"),
        (['{"kind": "run", "partial": false}', json.dumps(rec(A, "passed-ish"))], ":2: unknown outcome"),
        (['{"kind": "run", "partial": false}', json.dumps(rec(A, "skipped", "later"))], ":2: unknown skip category"),
        (['{"kind": "run", "partial": false}', '{"kind": "test", "nodeid": "x"}'], ":2: 'outcome' is missing"),
        (['{"kind": "run", "partial": false}', json.dumps(rec(A)), json.dumps(rec(A))], ":3: a second test record"),
        (['{"kind": "run", "partial": false}', '{"kind": "must-pass"}'], ":2: 'problem' is missing"),
    ],
    ids=[
        "missing",
        "empty",
        "not-json",
        "not-an-object",
        "blank-line",
        "no-header",
        "second-header",
        "partial-not-bool",
        "unknown-kind",
        "unknown-outcome",
        "unknown-skip",
        "missing-field",
        "duplicate-test",
        "must-pass-without-problem",
    ],
)
def test_malformed_report(
    files: tuple[Path, Path], capsys: pytest.CaptureFixture[str], lines: list[str] | None, message: str
) -> None:
    report, ratchet = files
    if lines is not None:
        report.write_text("".join(f"{line}\n" for line in lines))
    write_list(ratchet, A)
    code, out, err = check(capsys, report, ratchet)
    assert (code, out) == (2, [])
    assert err.startswith(f"check_ratchet: {report}")
    assert message in err


def run_tool(*args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(TOOL), *args], capture_output=True, text=True, env=env, timeout=30, check=False
    )


def test_cli_exit_codes(tmp_path: Path) -> None:
    ratchet = write_list(tmp_path / "rust-passing.txt", A)
    holding = write_report(tmp_path / "holding.jsonl", rec(A))
    broken = write_report(tmp_path / "broken.jsonl", rec(A, "failed"))
    env = {"PATH": "/usr/bin:/bin"}
    result = run_tool("--report", str(holding), "--list", str(ratchet), env=env)
    assert (result.returncode, result.stdout) == (0, "ratchet holds: 1 listed id(s), full run\n")
    result = run_tool("--report", str(broken), "--list", str(ratchet), env=env)
    assert result.returncode == 1
    assert result.stdout.splitlines()[0] == f"BROKEN failed: {A}"
    result = run_tool("--report", str(tmp_path / "absent.jsonl"), "--list", str(ratchet), env=env)
    assert result.returncode == 2 and "absent.jsonl" in result.stderr
    assert run_tool("--list", str(ratchet), env=env).returncode == 2  # --report is required
    assert run_tool("--report", str(holding), "--bogus", env=env).returncode == 2


def test_cli_defaults_to_the_shipped_list(tmp_path: Path) -> None:
    """The committed list parses, and is what `--list` defaults to."""
    shipped = TOOLS.parent / "rust-passing.txt"
    assert cr.DEFAULT_LIST == shipped
    listed = cr.read_list(shipped)
    assert listed.header and all(line.startswith("# ") for line in listed.header)
    report = write_report(tmp_path / "report.jsonl", *(rec(nodeid) for nodeid in listed.ids), partial=True)
    result = run_tool("--report", str(report), env={"PATH": "/usr/bin:/bin"})
    assert result.returncode == 0, result.stderr
    assert result.stdout.endswith("partial run\n")
