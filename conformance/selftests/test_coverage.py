import subprocess
import sys
import textwrap
from pathlib import Path
from typing import Any

import pytest
import yaml
from agterm_conformance.coverage import Catalog, CatalogError, Declaration, evaluate, load_baseline, load_catalog

CONFORMANCE = Path(__file__).resolve().parents[1]

CATALOG = {
    "upstream": "v0.36.0",
    "commands": {
        "version": {"status": "uncovered"},
        "tree": {"status": "covered"},
        "zmx.present": {"status": "uncovered", "mode": "presentation-stream"},
        "old.cmd": {"status": "removed-upstream"},
        "debug.appearance": {"status": "uncovered"},
    },
    "fields": {"tree.session.splitRatio": {"status": "uncovered"}},
    "events": {"pane.split": {"status": "uncovered"}},
    "cli": {"session resize": {"status": "uncovered", "wire": "session.resize"}},
    "behaviors": {"keymap.leader-chord": {"status": "uncovered"}},
    "exemptions": {"debug.appearance": "refused outside UI-test mode"},
}
UI_IDS = {
    "upstream": "v0.36.0",
    "ids": {"ask-dialog": {"status": "uncovered", "at": ["a.swift:1"]}, "ask-button-{choice}": {"status": "uncovered"}},
    "manual": {"welcome-skill-checkbox": {"status": "uncovered", "site": "agterm/WelcomeAlert.swift:identifier"}},
}
EVERYTHING = (
    "version",
    "tree",
    "zmx.present",
    "field:tree.session.splitRatio",
    "event:pane.split",
    "cli:session resize",
    "behavior:keymap.leader-chord",
    "ui:ask-dialog",
    "ui:ask-button-{choice}",
    "ui:welcome-skill-checkbox",
)


def write_catalog(directory: Path, catalog: Any = None, ui_ids: Any = None) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "catalog.yaml"
    path.write_text(yaml.safe_dump(CATALOG if catalog is None else catalog, sort_keys=False))
    (directory / "ui-ids.yaml").write_text(yaml.safe_dump(UI_IDS if ui_ids is None else ui_ids, sort_keys=False))
    return path


@pytest.fixture
def catalog(tmp_path: Path) -> Catalog:
    path = write_catalog(tmp_path)
    return load_catalog(path, tmp_path / "ui-ids.yaml")


def with_exemptions(catalog: Catalog, exemptions: dict[str, Any]) -> Catalog:
    return Catalog(catalog.items, exemptions)


def test_full_coverage(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", EVERYTHING, streams=("zmx.present",))])
    assert report.ok, report.failure_text()
    assert report.uncovered == [] and report.errors == []
    assert report.sections["commands"].line("commands") == (
        "commands: 3/4 covered (75.0%), 1 exempted, 0 baselined, 0 uncovered"
    )
    assert report.sections["ui"].line("ui") == "ui: 3/3 covered (100.0%), 0 exempted, 0 baselined, 0 uncovered"


def test_uncovered_items_fail_and_are_listed(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", ("version", "ui:ask-dialog"))])
    assert not report.ok
    assert report.uncovered == [
        "tree",
        "zmx.present",
        "field:tree.session.splitRatio",
        "event:pane.split",
        "cli:session resize",
        "behavior:keymap.leader-chord",
        "ui:ask-button-{choice}",
        "ui:welcome-skill-checkbox",
    ]
    assert "8 uncovered" in report.failure_text()
    assert "  cli:session resize" in report.failure_text().splitlines()


def test_status_does_not_decide_coverage(catalog: Catalog) -> None:
    # `tree` says covered in the catalog, `version` uncovered; only markers count.
    report = evaluate(catalog, [Declaration("t", ("version",))])
    assert "tree" in report.uncovered and "version" not in report.uncovered


@pytest.mark.parametrize(
    ("target", "message"),
    [
        ("session.bogus", "covers unknown item 'session.bogus'"),
        ("field:session.splitRatio", "covers unknown item 'field:session.splitRatio'"),
        ("splitRatio", "covers unknown item 'splitRatio'"),
        ("ui:ask-button-yes", "covers unknown item 'ui:ask-button-yes'"),
        ("old.cmd", "covers 'old.cmd', which is removed-upstream"),
    ],
)
def test_unknown_or_removed_covers_target(catalog: Catalog, target: str, message: str) -> None:
    report = evaluate(catalog, [Declaration("tests/test_x.py::test_y", (*EVERYTHING, target), ("zmx.present",))])
    assert report.uncovered == []
    assert report.errors == [f"tests/test_x.py::test_y: {message}"]
    assert not report.ok


def test_removed_items_are_not_required(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", EVERYTHING, streams=("zmx.present",))])
    assert "old.cmd" not in report.uncovered
    assert report.sections["commands"].active == 4


@pytest.mark.parametrize("reason", ["", "   ", None, 3])
def test_exemption_without_reason(catalog: Catalog, reason: Any) -> None:
    exempt = with_exemptions(catalog, {"debug.appearance": reason})
    report = evaluate(exempt, [Declaration("t", EVERYTHING, streams=("zmx.present",))])
    assert report.errors == ["exemption 'debug.appearance' needs a non-empty reason"]
    assert report.uncovered == ["debug.appearance"]


@pytest.mark.parametrize(
    ("key", "message"),
    [
        ("nope", "exemption 'nope' names no catalog item"),
        ("ui:nope", "exemption 'ui:nope' names no catalog item"),
        ("old.cmd", "exemption 'old.cmd' names an item that is removed-upstream"),
    ],
)
def test_exemption_must_name_an_active_item(catalog: Catalog, key: str, message: str) -> None:
    exempt = with_exemptions(catalog, {"debug.appearance": "why", key: "why"})
    report = evaluate(exempt, [Declaration("t", EVERYTHING, streams=("zmx.present",))])
    assert report.errors == [message]


def test_exempted_item_also_covered_counts_once_as_covered(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", (*EVERYTHING, "debug.appearance"), ("zmx.present",))])
    assert report.ok
    assert (report.sections["commands"].covered, report.sections["commands"].exempted) == (4, 0)


def test_stream_command_needs_a_stream_declaration(catalog: Catalog) -> None:
    targets = tuple(t for t in EVERYTHING if t != "zmx.present")
    refusal = Declaration("tests/test_zmx.py::test_refused", ("zmx.present",))
    report = evaluate(catalog, [Declaration("t", targets), refusal])
    assert report.uncovered == ["zmx.present"]
    assert report.errors == []
    assert "zmx.present is named only without streams=('zmx.present',): tests/test_zmx.py::test_refused" in (
        report.failure_text()
    )
    driven = Declaration("tests/test_zmx.py::test_stream", ("zmx.present",), ("zmx.present",))
    assert evaluate(catalog, [Declaration("t", targets), refusal, driven]).ok


def test_streams_on_a_command_without_a_mode(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", EVERYTHING, streams=("zmx.present", "version"))])
    assert report.errors == ["t: streams names 'version', which has no stream mode"]


def test_baseline_accepts_listed_uncovered_items(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", ("version",))], baseline=[t for t in EVERYTHING if t != "version"])
    assert report.ok, report.failure_text()
    assert report.uncovered == []
    assert "tree" in report.baselined and len(report.baselined) == 9
    assert report.sections["commands"].line("commands") == (
        "commands: 1/4 covered (25.0%), 1 exempted, 2 baselined, 0 uncovered"
    )


def test_baseline_does_not_accept_unlisted_items(catalog: Catalog) -> None:
    report = evaluate(catalog, [Declaration("t", ("version",))], baseline=["tree"])
    assert "zmx.present" in report.uncovered and "tree" not in report.uncovered
    assert not report.ok


@pytest.mark.parametrize(
    ("entry", "message"),
    [
        ("version", "baseline entry 'version' is covered: delete it"),
        ("debug.appearance", "baseline entry 'debug.appearance' is exempted: delete it"),
        ("old.cmd", "baseline entry 'old.cmd' is removed-upstream: delete it"),
        ("session.bogus", "baseline entry 'session.bogus' names no catalog item"),
    ],
)
def test_stale_baseline_entry_fails(catalog: Catalog, entry: str, message: str) -> None:
    rest = [t for t in EVERYTHING if t != "version"]
    report = evaluate(catalog, [Declaration("t", ("version",))], baseline=[*rest, entry])
    assert report.errors == [message]


def test_duplicate_baseline_entry_fails(catalog: Catalog) -> None:
    rest = [t for t in EVERYTHING if t != "version"]
    report = evaluate(catalog, [Declaration("t", ("version",))], baseline=[*rest, "tree"])
    assert report.errors == ["baseline lists 'tree' 2 times"]


def test_load_baseline_skips_comments_and_blanks(tmp_path: Path) -> None:
    path = tmp_path / "baseline.txt"
    path.write_text("# header\n\ntree\ncli:session resize  # trailing\n  ui:ask-dialog\n")
    assert load_baseline(path) == ["tree", "cli:session resize", "ui:ask-dialog"]


@pytest.mark.parametrize(
    ("catalog", "ui_ids", "message"),
    [
        ({**CATALOG, "commands": {"tree": "covered"}}, None, "commands.tree needs a status"),
        ({**CATALOG, "fields": ["x"]}, None, "`fields` is not a mapping"),
        (["x"], None, "not a mapping"),
        (
            None,
            {**UI_IDS, "manual": {"ask-dialog": {"status": "uncovered", "site": "a:b"}}},
            "ui:ask-dialog is listed twice",
        ),
    ],
)
def test_malformed_catalog(tmp_path: Path, catalog: Any, ui_ids: Any, message: str) -> None:
    path = write_catalog(tmp_path, catalog, ui_ids)
    with pytest.raises(CatalogError, match=message):
        load_catalog(path, tmp_path / "ui-ids.yaml")


# -- the plugin side


def gate_project(pytester: pytest.Pytester, tests: dict[str, str], catalog: Any = None) -> Path:
    path = write_catalog(pytester.path, catalog)
    for name, source in tests.items():
        target = pytester.path / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(textwrap.dedent(source))
    return path


COVERS_ALL = f"""
import pytest

@pytest.mark.covers(*{EVERYTHING!r}, streams=("zmx.present",))
def test_everything(ctl):
    raise AssertionError("a conformance test ran under --coverage-gate")
"""


def test_gate_runs_no_conformance_test(pytester: pytest.Pytester) -> None:
    catalog = gate_project(pytester, {"tests/test_all.py": COVERS_ALL})
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog))
    result.assert_outcomes(passed=1)
    result.stdout.fnmatch_lines(
        [
            "*agterm coverage*",
            "commands: 3/4 covered (75.0%), 1 exempted, 0 baselined, 0 uncovered",
            "ui: 3/3 covered (100.0%), 0 exempted, 0 baselined, 0 uncovered",
        ]
    )


def test_gate_fails_listing_uncovered(pytester: pytest.Pytester) -> None:
    catalog = gate_project(
        pytester,
        {
            "tests/test_one.py": """
            import pytest

            @pytest.mark.covers("version", "zmx.present", "field:nope")
            def test_one():
                pass
            """
        },
    )
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog))
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(
        [
            "*9 uncovered (cover, exempt with a reason, or baseline):",
            "  tree",
            "  zmx.present",
            "*zmx.present is named only without streams=('zmx.present',): tests/test_one.py::test_one",
            "*1 declaration/catalog errors:",
            "  tests/test_one.py::test_one: covers unknown item 'field:nope'",
        ]
    )


def test_gate_with_baseline(pytester: pytest.Pytester) -> None:
    catalog = gate_project(
        pytester, {"tests/test_one.py": '@__import__("pytest").mark.covers("version")\ndef test_one(): pass\n'}
    )
    baseline = pytester.path / "baseline.txt"
    baseline.write_text("\n".join(t for t in EVERYTHING if t != "version") + "\n")
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog), "--coverage-baseline", str(baseline))
    result.assert_outcomes(passed=1)
    result.stdout.fnmatch_lines(["baselined, accepted for now (9):", "  tree"])
    baseline.write_text(baseline.read_text() + "version\n")
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog), "--coverage-baseline", str(baseline))
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["  baseline entry 'version' is covered: delete it"])


def test_gate_reads_declarations_only_under_tests(pytester: pytest.Pytester) -> None:
    outside = COVERS_ALL.replace("test_everything", "test_elsewhere")
    catalog = gate_project(pytester, {"selftests/test_other.py": outside, "tests/test_none.py": "def test_x(): pass\n"})
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog))
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["*10 uncovered*"])


@pytest.mark.parametrize(
    "args", [["-k", "nothing"], ["-m", "nothing"], ["--deselect", "tests/test_all.py::test_everything"]]
)
def test_gate_reads_declarations_before_deselection(pytester: pytest.Pytester, args: list[str]) -> None:
    catalog = gate_project(pytester, {"tests/test_all.py": COVERS_ALL})
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog), *args)
    result.assert_outcomes(passed=1, deselected=1)


@pytest.mark.parametrize(
    ("args", "message"),
    [
        (["tests/sub"], "*collect */tests or a parent of it*"),
        (["tests/test_all.py::test_everything"], "*tests/test_all.py::test_everything selects one*"),
        (["--ignore", "tests/sub"], "*drop --ignore*"),
        (["--ignore-glob", "*sub*"], "*drop --ignore-glob*"),
        (["--lf"], "*drop --lf*"),
    ],
)
def test_gate_refuses_a_partial_collection(pytester: pytest.Pytester, args: list[str], message: str) -> None:
    catalog = gate_project(pytester, {"tests/test_all.py": COVERS_ALL, "tests/sub/test_x.py": "def test_x(): pass\n"})
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog), *args)
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([message])


def test_gate_default_catalog_is_in_the_rootdir(pytester: pytest.Pytester) -> None:
    gate_project(pytester, {"tests/test_all.py": COVERS_ALL})
    pytester.makeini("[pytest]\n")
    pytester.runpytest("--coverage-gate").assert_outcomes(passed=1)


def test_gate_missing_files(pytester: pytest.Pytester) -> None:
    catalog = gate_project(pytester, {"tests/test_all.py": COVERS_ALL})
    result = pytester.runpytest("--coverage-gate", "--catalog", str(pytester.path / "nope.yaml"))
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--catalog */nope.yaml is not a file*"])
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog), "--coverage-baseline", "nope.txt")
    result.stderr.fnmatch_lines(["*--coverage-baseline nope.txt is not a file*"])


def test_gate_malformed_catalog_is_a_usage_error(pytester: pytest.Pytester) -> None:
    catalog = gate_project(pytester, {"tests/test_all.py": COVERS_ALL}, {**CATALOG, "commands": {"tree": "x"}})
    result = pytester.runpytest("--coverage-gate", "--catalog", str(catalog))
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*commands.tree needs a status*"])


@pytest.mark.parametrize(
    ("marker", "message"),
    [
        ("covers()", "*covers takes one or more item strings*"),
        ('covers("")', "*covers takes one or more item strings*"),
        ("covers(3)", "*covers takes one or more item strings*"),
        ('covers("tree", stream=True)', "*covers takes no stream*"),
        ('covers("tree", streams="zmx.present")', "*covers streams must be a tuple of command names*"),
        ('covers("zmx.present", streams=("zmx.present", "zmx.present"))', "*covers streams lists a command twice*"),
        ('covers("tree", streams=("zmx.present",))', "*covers streams names zmx.present without covering it*"),
    ],
)
@pytest.mark.parametrize("gate", [True, False])
def test_malformed_covers_marker(pytester: pytest.Pytester, marker: str, message: str, gate: bool) -> None:
    catalog = gate_project(
        pytester, {"tests/test_bad.py": f"import pytest\n\n@pytest.mark.{marker}\ndef test_bad(): pass\n"}
    )
    result = pytester.runpytest(*(["--coverage-gate", "--catalog", str(catalog)] if gate else []))
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([message])


def test_committed_catalog_passes_the_ci_gate() -> None:
    """The command conformance-gate.yml runs, so a stale baseline line fails here first."""
    command = [sys.executable, "-m", "pytest", "conformance", "--coverage-gate"]
    command += ["--coverage-baseline", "conformance/coverage-baseline.txt", "-p", "no:cacheprovider", "-q"]
    result = subprocess.run(command, cwd=CONFORMANCE.parent, capture_output=True, text=True, check=False)
    assert result.returncode == 0, result.stdout[-4000:] + result.stderr[-2000:]
    assert "1 passed" in result.stdout
