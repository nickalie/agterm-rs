"""`font.inc`/`font.dec`/`font.reset` read back as each pane's live size (`fontSize`, `splitFontSize`,
`scratchFontSize`), and the theme slots (`theme.set`, read back by `theme.list`). The module shares one instance:
font tests close the sessions they open and the theme tests put back the slots they found. Theme settings land in
the instance's own state dir, never the account's config."""

import pytest
from agterm_conformance.match import assert_absent, assert_error, assert_ok, find_session

UNKNOWN = "zzz"
SIZE_FIELD = {"left": "fontSize", "right": "splitFontSize", "scratch": "scratchFontSize"}
LIGHT, DARK = "Dracula", "Nord"


def _node(ctl, sid):
    return find_session(ctl.tree(), sid)


def _size(ctl, sid, pane="left"):
    return _node(ctl, sid)[SIZE_FIELD[pane]]


def _settled_size(ctl, sid, pane, size):
    """Poll `tree` until `pane` of `sid` reads `size`: the new cell size reaches the projection after the reply."""

    def reads(tree):
        assert find_session(tree, sid).get(SIZE_FIELD[pane]) == size
        return True

    reads.__qualname__ = f"{SIZE_FIELD[pane]} of {sid} reading {size}"
    ctl.wait_tree(reads)


def _font(ctl, cmd, sid, pane=None):
    response = ctl.request(cmd, sid, **({} if pane is None else {"pane": pane}))
    assert assert_ok(response) == {"id": sid}, response.raw


@pytest.fixture
def opened(ctl):
    """Sessions a test opens, reset and closed afterwards: a new pane inherits the focused pane's size, and the
    Linux fork's `theme.set` stops answering once about seven sessions are mounted."""
    sids = []
    yield sids
    for sid in sids:
        for pane in SIZE_FIELD:
            ctl.request("font.reset", sid, pane=pane)
        ctl.request("session.close", sid)


@pytest.fixture
def fresh(ctl, opened):
    """A new selected session with its main pane up."""
    sid = assert_ok(ctl.request("session.new"))["id"]
    opened.append(sid)
    ctl.wait_mounted(sid, "left")
    return sid


# -- font


@pytest.mark.covers("font.inc", "font.dec", "font.reset", "field:tree.session.fontSize", "field:result.id")
def test_font_steps_the_main_pane_by_one_point_and_resets(ctl, fresh):
    base = _size(ctl, fresh)
    for cmd, size in [("font.inc", base + 1), ("font.inc", base + 2), ("font.dec", base + 1), ("font.reset", base)]:
        _font(ctl, cmd, fresh)
        _settled_size(ctl, fresh, "left", size)
    _font(ctl, "font.dec", fresh, pane="left")
    _settled_size(ctl, fresh, "left", base - 1)
    _font(ctl, "font.reset", fresh, pane="primary")
    _settled_size(ctl, fresh, "left", base)


@pytest.mark.covers("font.inc", "field:tree.session.fontSize", "behavior:protocol.target-resolution")
def test_font_without_a_target_steps_the_active_session_alone(ctl, opened, fresh):
    other = assert_ok(ctl.request("session.new", noSelect=True))["id"]
    opened.append(other)
    ctl.wait_tree(lambda tree: find_session(tree, fresh).get("active") is True)
    base = _size(ctl, fresh)
    assert assert_ok(ctl.request("font.inc")) == {"id": fresh}
    _settled_size(ctl, fresh, "left", base + 1)
    assert _size(ctl, fresh) == base + 1
    assert _node(ctl, other).get("fontSize") in (None, base)


@pytest.mark.covers("font.inc", "font.reset", "field:tree.session.splitFontSize", "field:tree.session.fontSize")
def test_font_pane_right_steps_the_split_alone(ctl, fresh):
    assert_ok(ctl.request("session.split", fresh, mode="on"))
    ctl.wait_mounted(fresh, "right")
    base = _size(ctl, fresh)
    assert _size(ctl, fresh, "right") == base
    _font(ctl, "font.inc", fresh, pane="right")
    _settled_size(ctl, fresh, "right", base + 1)
    _font(ctl, "font.inc", fresh, pane="bottom")
    _settled_size(ctl, fresh, "right", base + 2)
    assert _size(ctl, fresh) == base
    _font(ctl, "font.reset", fresh, pane="split")
    _settled_size(ctl, fresh, "right", base)


@pytest.mark.covers("font.dec", "font.reset", "field:tree.session.scratchFontSize", "field:tree.session.fontSize")
def test_font_pane_scratch_steps_the_scratch_terminal_alone(ctl, fresh):
    assert_ok(ctl.request("session.scratch", fresh, mode="on"))
    ctl.wait_mounted(fresh, "scratch")
    base = _size(ctl, fresh)
    assert _size(ctl, fresh, "scratch") == base
    _font(ctl, "font.dec", fresh, pane="scratch")
    _settled_size(ctl, fresh, "scratch", base - 1)
    assert _size(ctl, fresh) == base
    _font(ctl, "font.reset", fresh, pane="scratch")
    _settled_size(ctl, fresh, "scratch", base)
    assert_ok(ctl.request("session.scratch", fresh, mode="off"))


@pytest.mark.covers("font.inc", "font.dec", "font.reset")
@pytest.mark.parametrize("cmd", ["font.inc", "font.dec", "font.reset"])
@pytest.mark.parametrize(
    ("target", "pane", "error"),
    [
        ("fresh", "right", "session has no split pane"),
        ("fresh", "scratch", "session has no scratch terminal"),
        ("fresh", "middle", "invalid pane: middle"),
        (UNKNOWN, None, f"no such session: {UNKNOWN}"),
    ],
)
def test_font_refusals(ctl, fresh, cmd, target, pane, error):
    base = _size(ctl, fresh)
    args = {} if pane is None else {"pane": pane}
    assert_error(ctl.request(cmd, fresh if target == "fresh" else target, **args), error)
    assert _size(ctl, fresh) == base


# -- theme


@pytest.fixture
def theme_restored(ctl):
    """Put the theme slots back as the test found them."""
    found = assert_ok(ctl.request("theme.list"))
    yield found
    if found.get("sync"):
        ctl.request("theme.set", light=found["light"], dark=found["dark"])
    elif found.get("theme"):
        ctl.request("theme.set", name=found["theme"], dark="none")
    else:
        ctl.request("theme.set")


def _slots(adapter, result, light, dark=None):
    """Check a `theme.set`/`theme.list` result for the stored `light` (or single) and `dark` slots."""
    sync = dark is not None
    assert result["sync"] is sync, result
    if adapter.has("theme-slots-by-sync"):
        if sync:
            assert (result["light"], result["dark"]) == (light, dark), result
            assert_absent(result, "theme")
        else:
            assert result.get("theme") == light, result
            assert_absent(result, "light", "dark")
        return
    # The Linux fork always reports the stored light slot, and as `theme` the side on screen, which follows the
    # desktop's color scheme while syncing.
    assert result.get("light") == light, result
    assert result.get("dark") == dark, result
    assert result.get("theme") in ((light, dark) if sync else (light,)), result


def _set(ctl, adapter, slots=(None, None), **args):
    """`theme.set` with `args`, expecting the stored (light, dark) `slots`, and its `theme.list` read-back."""
    result = assert_ok(ctl.request("theme.set", **args))
    _slots(adapter, result, *slots)
    listed = assert_ok(ctl.request("theme.list"))
    assert {key: value for key, value in listed.items() if key != "themes"} == result
    return result


@pytest.mark.covers("theme.list", "field:result.theme", "field:result.themes", "field:result.sync")
def test_list_names_the_bundled_themes(ctl, theme_restored):
    themes = theme_restored["themes"]
    assert isinstance(themes, list) and all(isinstance(name, str) for name in themes)
    assert {LIGHT, DARK} <= set(themes)
    assert isinstance(theme_restored["sync"], bool)
    if "theme" in theme_restored:
        assert theme_restored["theme"] in themes


@pytest.mark.covers("theme.set", "theme.list", "field:result.theme", "field:result.sync")
def test_set_a_single_theme(ctl, adapter, theme_restored):
    _set(ctl, adapter, (LIGHT, None), name=LIGHT)
    _set(ctl, adapter, (DARK, None), name=f"  {DARK}  ")
    _set(ctl, adapter, (LIGHT, None), light=LIGHT)


@pytest.mark.covers("theme.set", "theme.list", "field:result.light", "field:result.dark", "field:result.sync")
def test_dark_slot_turns_appearance_sync_on_and_none_turns_it_off(ctl, adapter, theme_restored):
    _set(ctl, adapter, (LIGHT, None), name=LIGHT)
    _set(ctl, adapter, (LIGHT, DARK), dark=DARK)
    _set(ctl, adapter, (DARK, LIGHT), light=DARK, dark=LIGHT)
    _set(ctl, adapter, (DARK, None), dark="NONE")
    _set(ctl, adapter, (LIGHT, DARK), name=LIGHT, dark=DARK)
    _set(ctl, adapter, (DARK, None), name=DARK, dark="none")


@pytest.mark.covers("theme.set", "field:result.sync")
def test_bare_set_returns_to_ghostty_builtin_colors(ctl, adapter, theme_restored):
    _set(ctl, adapter, (LIGHT, DARK), light=LIGHT, dark=DARK)
    assert _set(ctl, adapter) == {"sync": False}


@pytest.mark.covers("theme.set")
@pytest.mark.parametrize(
    ("args", "error"),
    [
        ({"name": LIGHT, "light": DARK}, "theme.set takes either a name or --light, not both"),
        ({"name": "No Such Theme"}, "unknown theme: No Such Theme"),
        ({"light": LIGHT, "dark": "No Such Theme"}, "unknown theme: No Such Theme"),
        ({"name": LIGHT.lower()}, f"unknown theme: {LIGHT.lower()}"),
    ],
)
def test_set_refusals_leave_the_slots_alone(ctl, adapter, theme_restored, args, error):
    before = _set(ctl, adapter, (DARK, None), name=DARK)
    assert_error(ctl.request("theme.set", **args), error)
    assert {key: value for key, value in assert_ok(ctl.request("theme.list")).items() if key != "themes"} == before
