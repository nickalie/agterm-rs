import importlib.util
import shutil
import subprocess
import sys
from pathlib import Path
from types import ModuleType
from typing import Any

import pytest
import yaml

TOOLS = Path(__file__).resolve().parent.parent / "tools"
MINI = Path(__file__).with_name("fixtures") / "upstream-mini"
PROTOCOL = Path("agtermCore/Sources/agtermCore/ControlProtocol.swift")
PROJECTION = Path("agtermCore/Sources/agtermCore/ControlProjection.swift")
ASK = Path("agterm/Views/AskDialogView.swift")
WELCOME = Path("agterm/WelcomeAlert.swift")


def _load(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module  # dataclasses resolve their module by name
    spec.loader.exec_module(module)
    return module


gc = _load("gen_catalog")

COMMANDS = [
    "tree",
    "events.read",
    "session.resize",
    "font.inc",
    "font.dec",
    "font.reset",
    "quick",
    "session.background",
    "session.hud.open",
    "session.overlay.open",
    "session.overlay.navigate",
    "zmx.tree",
    "zmx.attach",
    "zmx.present",
    "session.overlay.job.run",
    "version",
    "debug.appearance",
]
FIELDS = [
    "result.id",
    "result.ratio",
    "result.tree",
    "result.note",
    "tree.workspaces",
    "tree.sidebarVisible",
    "tree.workspace.id",
    "tree.workspace.sessions",
    "tree.session.id",
    "tree.session.splitRatio",
    "tree.session.hud",
    "window.id",
    "window.open",
    "window.minimized",
]
CLI = {
    "tree": {"wire": "tree"},
    "events": {"wire": "events.read"},
    "session resize": {"wire": "session.resize"},
    "font inc": {"wire": "font.inc"},
    "font dec": {"wire": "font.dec"},
    "font reset": {"wire": "font.reset"},
    "quick": {"wire": "quick"},
    "session background image": {"wire": "session.background"},
    "session background color": {"wire": "session.background"},
    "session background clear": {"wire": "session.background"},
    "session hud": {"wire": "session.hud.open"},
    "session overlay open": {"wire": "session.overlay.open"},
    "session overlay navigate": {"wire": "session.overlay.navigate"},
    "zmx tree": {"wire": "zmx.tree"},
    "zmx attach": {"wire": "zmx.attach"},
    "zmx present": {"wire": "zmx.present"},
    "session overlay run-job": {"wire": "session.overlay.job.run"},
    "clipboard set": {},
    "version": {"wire": "version"},
}


@pytest.fixture
def mini(tmp_path: Path) -> Path:
    tree = tmp_path / "upstream"
    shutil.copytree(MINI, tree)
    return tree


def edit(tree: Path, rel: Path, old: str, new: str) -> None:
    path = tree / rel
    text = path.read_text()
    assert old in text
    path.write_text(text.replace(old, new, 1))


def problems(tree: Path) -> list[str]:
    return gc.generate(tree).problems


def test_commands_keep_declaration_order_and_implicit_raw_values() -> None:
    gen = gc.generate(MINI)
    assert gen.problems == []
    assert list(gen.commands) == COMMANDS


def test_stream_commands_carry_their_mode() -> None:
    gen = gc.generate(MINI)
    assert gen.commands["zmx.present"] == {"mode": "presentation-stream"}
    assert gen.commands["session.overlay.job.run"] == {"mode": "job-stream"}
    assert gen.commands["zmx.tree"] == {}
    assert gen.commands["zmx.attach"] == {}


def test_fields_follow_stored_properties_and_coding_keys() -> None:
    gen = gc.generate(MINI)
    assert list(gen.fields) == FIELDS


def test_events_from_the_event_kind_enum() -> None:
    assert list(gc.generate(MINI).events) == ["status", "pane.split"]


def test_cli_subcommands_from_synopses() -> None:
    assert gc.generate(MINI).cli == CLI


def test_comments_and_strings_do_not_shift_nesting() -> None:
    src = 'let a = "/* {" // } trailing\n/* { */ let b = 1'
    text = gc.strip_comments(src)
    assert len(text) == len(src)
    assert [line.strip() for line in text.splitlines()] == ['let a = "/* {"', "let b = 1"]
    assert gc._depth_between(text, 0, len(text)) == 0


@pytest.mark.parametrize(
    ("rel", "old", "new", "expected"),
    [
        (PROTOCOL, "    case quick\n", "    case quick(Int)\n", "ControlProtocol.swift:10: unparseable enum case"),
        (PROTOCOL, "    case quick\n", '    case quick = "a" + "b"\n', "unparseable enum case"),
        (PROTOCOL, "    public var id: String?\n", "    public var (id, x): (String, Int)\n", "unparseable property"),
        (
            PROJECTION,
            "    public let workspaces: [ControlWorkspaceNode]\n",
            "    public let workspaces: [ControlWorkspaceNode]\n    public func encode(to encoder: Encoder) throws {}\n",
            "custom encode(to:)",
        ),
        (PROJECTION, "public struct ControlTree:", "public struct ControlTreeNode:", "type ControlTree not found"),
        (PROTOCOL, "    public var id: String?\n", "    public let c, id: String?\n", "unparseable property"),
        (PROTOCOL, "    public var id: String?\n", "    public consuming var id: String?\n", "unparseable property"),
        (
            PROTOCOL,
            "    public var id: String?\n",
            "    public var id: String?,\n        extra: Int?\n",
            "property declaration continues on the next line",
        ),
        (
            PROJECTION,
            "    public var hasHud: Bool { hud != nil }\n",
            "    enum CodingKeys: String, CodingKey { case id }\n",
            "CodingKeys declared in an extension",
        ),
        (PROJECTION, "        case minimized\n", "        case minimized(Bool)\n", "unparseable enum case"),
        (
            Path("site/commands.html"),
            "session.resize</div>",
            "session.resize.v2</div>",
            "wire name session.resize.v2 is not in the Command enum",
        ),
        (Path("site/commands.html"), "  agtermctl version", "  version", "synopsis does not start with agtermctl"),
        (
            PROTOCOL,
            '    case zmxPresent = "zmx.present"\n',
            "",
            "generator table names zmx.present, which is not in the Command enum",
        ),
        (
            PROTOCOL,
            '    case debugAppearance = "debug.appearance"\n',
            "",
            "generator table names debug.appearance, which is not in the Command enum",
        ),
    ],
)
def test_unparseable_input_is_reported(mini: Path, rel: Path, old: str, new: str, expected: str) -> None:
    edit(mini, rel, old, new)
    found = problems(mini)
    assert any(expected in p for p in found), found


def test_an_encoder_in_another_files_extension_is_reported(mini: Path) -> None:
    (mini / "agtermCore/Sources/agtermCore/Extra.swift").write_text(
        "extension ControlWindowNode {\n    public func encode(to encoder: Encoder) throws {}\n}\n"
    )
    assert any("Extra.swift:2: custom encode(to:)" in p for p in problems(mini))


def test_property_shapes_that_still_encode_or_do_not(mini: Path) -> None:
    edit(
        mini, PROTOCOL, "    public var id: String?\n", "    nonisolated(unsafe) public var id: String?, extra: Int?\n"
    )
    edit(
        mini,
        PROTOCOL,
        "    public var tree: ControlTree?\n",
        "    public var tree: ControlTree? {\n        didSet {}\n    }\n",
    )
    edit(
        mini,
        PROTOCOL,
        "    public var isEmpty: Bool { id == nil }\n",
        "    public var isEmpty: Bool\n    {\n        id == nil\n    }\n",
    )
    gen = gc.generate(mini)
    assert gen.problems == []
    assert [f for f in gen.fields if f.startswith("result.")] == [
        "result.id",
        "result.extra",
        "result.ratio",
        "result.tree",
        "result.note",
    ]


def test_a_duplicate_type_is_reported(mini: Path) -> None:
    (mini / "agtermCore/Sources/agtermCore/Extra.swift").write_text("public enum ControlEventKind { case x }\n")
    assert any("type ControlEventKind declared 2 times" in p for p in problems(mini))


def test_missing_sections_are_reported(mini: Path) -> None:
    (mini / "site/commands.html").write_text("<html><body><p>nothing</p></body></html>\n")
    assert "no cli found" in problems(mini)
    (mini / "site/commands.html").unlink()
    assert "site/commands.html not found" in problems(mini)


def test_unparseable_input_writes_nothing(mini: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    catalog = tmp_path / "catalog.yaml"
    catalog.write_text("upstream: v0.0.1\n")
    edit(mini, PROTOCOL, "    case quick\n", "    case quick(Int)\n")
    assert gc.main(["--source", str(mini), "--tag", "v0.36.0", "--catalog", str(catalog)]) == 1
    assert catalog.read_text() == "upstream: v0.0.1\n"
    assert "unparseable enum case" in capsys.readouterr().err


def generated(**sections: dict[str, dict[str, Any]]) -> Any:
    gen = gc.Generated()
    for name, items in sections.items():
        setattr(gen, name, items)
    return gen


def test_merge_preserves_status_manual_keys_and_hand_sections() -> None:
    old = {
        "upstream": "v0.35.0",
        "commands": {
            "tree": {"status": "covered", "note": "kept"},
            "zmx.present": {"status": "covered", "mode": "stale-mode"},
        },
        "fields": {"tree.app": {"status": "covered"}},
        "behaviors": {"restore.live": {"requires": ["live-sessions"]}},
        "exemptions": {"debug.appearance": "a manual reason", "cli:clipboard set": "manual"},
    }
    gen = generated(
        commands={"tree": {}, "zmx.present": {"mode": "presentation-stream"}, "new.cmd": {}},
        fields={"tree.app": {}},
        events={"status": {}},
        cli={"tree": {"wire": "tree"}},
    )
    catalog, changes = gc.merge(old, gen, "v0.36.0")
    assert catalog["upstream"] == "v0.36.0"
    assert catalog["commands"] == {
        "tree": {"status": "covered", "note": "kept"},
        "zmx.present": {"status": "covered", "mode": "presentation-stream"},
        "new.cmd": {"status": "uncovered"},
    }
    assert catalog["fields"] == {"tree.app": {"status": "covered"}}
    assert catalog["behaviors"] == {"restore.live": {"requires": ["live-sessions"]}, **gc.AUTO_BEHAVIORS}
    assert catalog["exemptions"]["debug.appearance"] == "a manual reason"
    assert catalog["exemptions"]["cli:clipboard set"] == "manual"
    assert catalog["exemptions"]["zmx.attach"] == gc.AUTO_EXEMPTIONS["zmx.attach"]
    assert changes.added == {"commands": ["new.cmd"], "events": ["status"], "cli": ["tree"]}
    assert changes.removed == {}


def test_merge_marks_vanished_items_and_revives_returning_ones() -> None:
    old = {
        "commands": {
            "gone.cmd": {"status": "covered", "note": "kept"},
            "long.gone": {"status": "removed-upstream"},
            "back.again": {"status": "removed-upstream"},
        },
        "cli": {"old sub": {"status": "uncovered", "wire": "gone.cmd"}},
    }
    gen = generated(commands={"back.again": {}}, cli={})
    catalog, changes = gc.merge(old, gen, "v0.36.0")
    assert catalog["commands"] == {
        "back.again": {"status": "uncovered"},
        "gone.cmd": {"status": "removed-upstream", "note": "kept"},
        "long.gone": {"status": "removed-upstream"},
    }
    assert catalog["cli"] == {"old sub": {"status": "removed-upstream", "wire": "gone.cmd"}}
    assert changes.removed == {"commands": ["gone.cmd"], "cli": ["old sub"]}


def test_a_fresh_catalog_auto_exempts_and_seeds_behaviors(mini: Path, tmp_path: Path) -> None:
    catalog = tmp_path / "catalog.yaml"
    assert gc.main(["--source", str(mini), "--tag", "v0.36.0", "--catalog", str(catalog)]) == 0
    data = yaml.safe_load(catalog.read_text())
    assert data["upstream"] == "v0.36.0"
    assert list(data["commands"]) == COMMANDS
    assert all(entry["status"] == "uncovered" for entry in data["commands"].values())
    assert data["exemptions"] == gc.AUTO_EXEMPTIONS
    assert data["behaviors"] == gc.AUTO_BEHAVIORS
    assert data["cli"]["clipboard set"] == {"status": "uncovered"}
    ui = yaml.safe_load((tmp_path / "ui-ids.yaml").read_text())
    assert ui == {
        "upstream": "v0.36.0",
        "ids": {k: {"status": "uncovered", **v} for k, v in UI_IDS.items()},
        "manual": {},
    }


def test_regeneration_is_byte_stable_and_keeps_edits(mini: Path, tmp_path: Path) -> None:
    catalog = tmp_path / "catalog.yaml"
    args = ["--source", str(mini), "--tag", "v0.36.0", "--catalog", str(catalog)]
    assert gc.main(args) == 0
    first, first_ui = catalog.read_text(), (tmp_path / "ui-ids.yaml").read_text()
    assert gc.main(args) == 0
    assert catalog.read_text() == first
    assert (tmp_path / "ui-ids.yaml").read_text() == first_ui
    catalog.write_text(first.replace("  tree: {status: uncovered}", "  tree: {status: covered}", 1))
    edit(mini, PROTOCOL, "    case quick\n", "")
    edit(mini, Path("site/commands.html"), "agtermctl quick [show|hide|toggle]", "agtermctl tree")
    edit(mini, Path("site/commands.html"), ">quick</div>", ">tree</div>")
    assert gc.main(args) == 0
    data = yaml.safe_load(catalog.read_text())
    assert data["commands"]["tree"] == {"status": "covered"}
    assert data["commands"]["quick"] == {"status": "removed-upstream"}
    assert list(data["commands"])[-1] == "quick"


def test_dump_writes_one_line_per_item() -> None:
    catalog = {
        "upstream": "v0.36.0",
        "commands": {"session.resize": {"status": "covered"}},
        "cli": {"session resize": {"status": "uncovered", "wire": "session.resize"}},
        "behaviors": {"restore.live": {"requires": ["live-sessions"]}},
        "exemptions": {"debug.appearance": "refused outside UI-test mode"},
    }
    text = gc.dump(catalog)
    assert text.startswith("# Generated by conformance/tools/gen_catalog.py from upstream v0.36.0.\n")
    assert "\n  session.resize: {status: covered}\n" in text
    assert "\n  session resize: {status: uncovered, wire: session.resize}\n" in text
    assert "\n  restore.live: {requires: [live-sessions]}\n" in text
    assert "\nexemptions:\n  debug.appearance: refused outside UI-test mode\n" in text
    assert yaml.safe_load(text) == catalog


def test_reads_the_tag_with_git_archive_without_touching_the_checkout(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    repo = tmp_path / "repo"
    shutil.copytree(MINI, repo)

    def git(*args: str) -> str:
        env = {"GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null", "PATH": "/usr/bin:/bin"}
        return subprocess.run(
            ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
            check=True,
            capture_output=True,
            text=True,
            env=env,
        ).stdout

    git("init", "-q")
    git("add", ".")
    git("commit", "-q", "-m", "mini")
    git("tag", "v0.36.0")
    edit(repo, PROTOCOL, "    case quick\n", "    case quick(Int)\n")  # a dirty tree must not leak into the tag
    head, status = git("rev-parse", "HEAD"), git("status", "--porcelain")
    catalog = tmp_path / "catalog.yaml"
    assert gc.main(["--repo", str(repo), "--tag", "v0.36.0", "--catalog", str(catalog)]) == 0
    assert list(yaml.safe_load(catalog.read_text())["commands"]) == COMMANDS
    assert (git("rev-parse", "HEAD"), git("status", "--porcelain")) == (head, status)
    out = capsys.readouterr().out
    assert "commands: 17 upstream, +17 new, -0 removed-upstream" in out
    assert "ids: 7 upstream, +7 new, -0 removed-upstream" in out
    assert gc.main(["--repo", str(repo), "--tag", "v9.9.9", "--catalog", str(catalog)]) == 1
    assert "git archive v9.9.9 failed" in capsys.readouterr().err


def test_the_committed_catalog_parses_and_names_its_tag() -> None:
    data = yaml.safe_load((TOOLS.parent / "catalog.yaml").read_text())
    assert data["upstream"] == "v0.36.0"
    assert len(data["commands"]) == 110
    assert set(gc.GENERATED_SECTIONS) | {"upstream", "behaviors", "exemptions"} == set(data)


UI_IDS = {
    "ask-dialog": {"at": [f"{ASK}:11", f"{ASK}:22"]},
    "ask-button-{choice}": {"at": [f"{ASK}:14"]},
    "command-palette": {"at": [f"{ASK}:17"]},
    "pick-palette": {"at": [f"{ASK}:17"]},
    "welcome-install": {"at": [f"{WELCOME}:6"]},
    "workspace-row": {"at": [f"{WELCOME}:12"]},
    "session-row": {"at": [f"{WELCOME}:12"]},
}


def test_ui_ids_from_literals_templates_ternaries_and_appkit_setters() -> None:
    gen = gc.generate(MINI)
    assert gen.problems == []
    assert gen.ui_ids == UI_IDS
    assert gen.ui_variables == {}


@pytest.mark.parametrize(
    ("call", "expected"),
    [
        ('.accessibilityIdentifier(makeID("x"))', 'unparseable identifier argument: makeID("x")'),
        ('.accessibilityIdentifier("row-" + suffix)', "unparseable identifier argument"),
        ('.accessibilityIdentifier(#"raw"#)', "unparseable identifier argument"),
        (".accessibilityIdentifier()", "unparseable identifier argument"),
        ('.accessibilityIdentifier(flag ? "a" : other)', "unparseable identifier argument"),
        ('.accessibilityIdentifier("a{b}")', 'unparseable identifier literal: "a{b}"'),
        ('.accessibilityIdentifier("a b")', "unparseable identifier literal"),
        ('.accessibilityIdentifier("row-\\(a + b)")', "unparseable identifier literal"),
        ('.accessibilityIdentifier("q\\"x")', "unparseable identifier literal"),
        ('view.setAccessibilityIdentifier(on ? "a-\\(f(x))" : "b")', "unparseable identifier literal"),
        ('.accessibilityIdentifier("open"', "unbalanced identifier call"),
    ],
)
def test_unparseable_identifiers_are_reported(mini: Path, call: str, expected: str) -> None:
    (mini / "agterm/Extra.swift").write_text(f'let v = Text("x")\n    {call}\n')
    found = problems(mini)
    assert any(f"agterm/Extra.swift:2: {expected}" in p for p in found), found


def test_no_ui_ids_is_reported(mini: Path) -> None:
    shutil.rmtree(mini / "agterm")
    assert "no ui ids found under agterm" in problems(mini)


def _variable_site(mini: Path) -> None:
    (mini / "agterm/Checkbox.swift").write_text(
        "func checkbox(identifier: String) -> NSButton {\n"
        "    let button = NSButton()\n"
        "    button.setAccessibilityIdentifier(identifier)\n"
        "    return button\n"
        "}\n"
    )


def _paths(tmp_path: Path) -> tuple[Path, Path, list[str]]:
    catalog, ui = tmp_path / "catalog.yaml", tmp_path / "ui-ids.yaml"
    return catalog, ui, ["--tag", "v0.36.0", "--catalog", str(catalog)]


def test_a_variable_identifier_needs_a_manual_entry(
    mini: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _variable_site(mini)
    catalog, ui, args = _paths(tmp_path)
    assert gc.generate(mini).ui_variables == {"agterm/Checkbox.swift:identifier": ["agterm/Checkbox.swift:3"]}
    assert gc.main(["--source", str(mini), *args]) == 1
    assert not catalog.exists() and not ui.exists()
    err = capsys.readouterr().err
    assert "agterm/Checkbox.swift:3: identifier passed through `identifier`" in err
    assert "site: agterm/Checkbox.swift:identifier" in err


def test_manual_entries_follow_their_site(mini: Path, tmp_path: Path) -> None:
    _variable_site(mini)
    _, ui, args = _paths(tmp_path)
    site = "agterm/Checkbox.swift:identifier"
    ui.write_text(
        yaml.safe_dump(
            {
                "manual": {
                    "skill-checkbox": {"site": site, "from": "line 9", "status": "covered"},
                    "hooks-checkbox": {"site": site},
                }
            },
            sort_keys=False,
        )
    )
    assert gc.main(["--source", str(mini), *args]) == 0
    manual = yaml.safe_load(ui.read_text())["manual"]
    assert manual == {
        "skill-checkbox": {"status": "covered", "site": site, "from": "line 9"},
        "hooks-checkbox": {"status": "uncovered", "site": site},
    }
    assert (
        "  skill-checkbox: {status: covered, site: 'agterm/Checkbox.swift:identifier', from: line 9}\n"
        in ui.read_text()
    )
    (mini / "agterm/Checkbox.swift").unlink()
    assert gc.main(["--source", str(mini), *args]) == 0
    assert {k: v["status"] for k, v in yaml.safe_load(ui.read_text())["manual"].items()} == {
        "skill-checkbox": "removed-upstream",
        "hooks-checkbox": "removed-upstream",
    }
    _variable_site(mini)
    assert gc.main(["--source", str(mini), *args]) == 0
    assert {v["status"] for v in yaml.safe_load(ui.read_text())["manual"].values()} == {"uncovered"}


@pytest.mark.parametrize(
    ("manual", "expected"),
    [
        ({"x": {"from": "line 1"}}, "manual id x: needs a site"),
        ({"x": "agterm/Checkbox.swift:identifier"}, "manual id x: needs a site"),
        ({"ask-dialog": {"site": "agterm/Checkbox.swift:identifier"}}, "manual id ask-dialog is also generated from"),
        (["x"], "expected a mapping with a `manual` mapping"),
    ],
)
def test_malformed_manual_entries_write_nothing(
    mini: Path, tmp_path: Path, capsys: pytest.CaptureFixture[str], manual: Any, expected: str
) -> None:
    catalog, ui, args = _paths(tmp_path)
    ui.write_text(yaml.safe_dump({"manual": manual}))
    before = ui.read_text()
    assert gc.main(["--source", str(mini), *args]) == 1
    assert ui.read_text() == before and not catalog.exists()
    assert expected in capsys.readouterr().err


def test_ui_ids_keep_status_and_mark_vanished_ids(mini: Path, tmp_path: Path) -> None:
    _, ui, args = _paths(tmp_path)
    ui.write_text(
        yaml.safe_dump(
            {
                "ids": {
                    "ask-dialog": {"status": "covered", "at": ["stale:1"], "note": "kept"},
                    "gone-id": {"status": "covered", "at": ["agterm/Old.swift:3"]},
                },
                "manual": {},
            }
        )
    )
    assert gc.main(["--source", str(mini), *args]) == 0
    ids = yaml.safe_load(ui.read_text())["ids"]
    assert ids["ask-dialog"] == {"status": "covered", "at": [f"{ASK}:11", f"{ASK}:22"], "note": "kept"}
    assert ids["gone-id"] == {"status": "removed-upstream", "at": ["agterm/Old.swift:3"]}
    assert list(ids)[-1] == "gone-id"


def test_ui_ids_path_can_be_given(mini: Path, tmp_path: Path) -> None:
    catalog, _, args = _paths(tmp_path)
    elsewhere = tmp_path / "out" / "ids.yaml"
    elsewhere.parent.mkdir()
    assert gc.main(["--source", str(mini), *args, "--ui-ids", str(elsewhere)]) == 0
    assert catalog.exists() and not (tmp_path / "ui-ids.yaml").exists()
    assert set(yaml.safe_load(elsewhere.read_text())["ids"]) == set(UI_IDS)


def test_the_committed_ui_ids_parse_and_name_their_tag() -> None:
    data = yaml.safe_load((TOOLS.parent / "ui-ids.yaml").read_text())
    assert data["upstream"] == "v0.36.0"
    assert len(data["ids"]) == 101
    assert len(data["manual"]) == 13
    assert all(entry["site"].startswith("agterm/") for entry in data["manual"].values())
    assert not set(data["ids"]) & set(data["manual"])
