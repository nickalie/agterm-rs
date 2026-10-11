import pytest

from agterm_conformance.client import Response
from agterm_conformance.match import (
    ANY,
    Unordered,
    active_session_id,
    active_workspace_id,
    assert_absent,
    assert_error,
    assert_ok,
    assert_subset,
    find_session,
    session_ids,
    window_ids,
    workspace_ids,
)


def response(raw: dict) -> Response:
    return Response(ok=raw["ok"], result=raw.get("result"), error=raw.get("error"), raw=raw)


TREE = {
    "tree": {
        "workspaces": [
            {"id": "w1", "name": "one", "active": False, "sessions": [{"id": "s1", "active": False}]},
            {
                "id": "w2",
                "name": "two",
                "active": True,
                "sessions": [{"id": "s2", "active": True, "splitRatio": 0.5}, {"id": "s3", "active": False}],
            },
        ],
        "sidebarVisible": True,
    }
}


def test_subset_match_ignores_extra_fields() -> None:
    assert_subset(TREE, {"tree": {"sidebarVisible": True, "workspaces": [{"id": "w1"}, {"name": "two"}]}})


def test_subset_missing_field() -> None:
    with pytest.raises(AssertionError, match=r"\$\.tree\.idleMs: missing"):
        assert_subset(TREE, {"tree": {"idleMs": 3}})


def test_subset_wrong_value_reports_path() -> None:
    with pytest.raises(AssertionError, match=r"\$\.tree\.workspaces\[1\]\.name: expected 'one', got 'two'"):
        assert_subset(TREE, {"tree": {"workspaces": [{}, {"name": "one"}]}})


def test_subset_list_length_and_order() -> None:
    with pytest.raises(AssertionError, match="expected 1 items, got 2"):
        assert_subset([1, 2], [1])
    with pytest.raises(AssertionError, match=r"\$\[0\]: expected 2, got 1"):
        assert_subset([1, 2], [2, 1])


def test_subset_unordered() -> None:
    assert_subset([{"id": "a", "n": 1}, {"id": "b"}], Unordered([{"id": "b"}, {"id": "a"}]))
    with pytest.raises(AssertionError, match="no order"):
        assert_subset(["a", "b"], Unordered(["a", "c"]))
    with pytest.raises(AssertionError, match="in any order"):
        assert_subset(["a"], Unordered(["a", "a"]))


def test_subset_unordered_needs_backtracking() -> None:
    assert_subset([{"id": "a", "x": 1}, {"id": "a"}], Unordered([{"id": "a"}, {"id": "a", "x": 1}]))


def test_subset_any_requires_presence() -> None:
    assert_subset({"id": None}, {"id": ANY})
    with pytest.raises(AssertionError, match="missing"):
        assert_subset({}, {"id": ANY})


def test_subset_float_tolerance() -> None:
    assert_subset({"ratio": 0.5 + 1e-7}, {"ratio": 0.5})
    assert_subset({"ratio": 0.31}, {"ratio": 0.3}, tol=0.02)
    with pytest.raises(AssertionError, match="expected 0.5, got 0.5001"):
        assert_subset({"ratio": 0.5001}, {"ratio": 0.5})


def test_subset_whole_double_encoded_as_int() -> None:
    assert_subset({"fontSize": 13}, {"fontSize": 13.0})


def test_subset_bool_is_not_a_number() -> None:
    with pytest.raises(AssertionError):
        assert_subset({"split": 1}, {"split": True})
    with pytest.raises(AssertionError):
        assert_subset({"count": False}, {"count": 0})


def test_subset_type_mismatch() -> None:
    with pytest.raises(AssertionError, match="expected an object"):
        assert_subset({"tree": []}, {"tree": {}})
    with pytest.raises(AssertionError, match="expected a list"):
        assert_subset({"items": "ab"}, {"items": ["a", "b"]})


def test_absent() -> None:
    assert_absent(TREE, "tree.idleMs", "tree.workspaces.1.sessions.1.splitRatio")


def test_absent_but_present() -> None:
    with pytest.raises(AssertionError, match="splitRatio: expected omitted, got 0.5"):
        assert_absent(TREE, "tree.workspaces.1.sessions.0.splitRatio")


def test_absent_null_counts_as_present() -> None:
    with pytest.raises(AssertionError, match="expected omitted, got None"):
        assert_absent({"hud": None}, "hud")


def test_absent_missing_parent_fails() -> None:
    with pytest.raises(AssertionError, match="no 'tre'"):
        assert_absent(TREE, "tre.idleMs")
    with pytest.raises(AssertionError, match="no '5'"):
        assert_absent(TREE, "tree.workspaces.5.hud")


def test_absent_unwraps_response() -> None:
    assert_absent(response({"ok": True, "result": TREE}), "tree.idleMs")


def test_assert_ok() -> None:
    assert assert_ok(response({"ok": True, "result": {"a": 1}})) == {"a": 1}
    assert assert_ok(response({"ok": True})) == {}
    with pytest.raises(AssertionError, match="'no such session'"):
        assert_ok(response({"ok": False, "error": "no such session"}))


def test_error_allows_result() -> None:
    assert_error(response({"ok": False, "error": "cursor expired", "result": {"events": {}}}), "cursor expired")


def test_error_text_only_when_given() -> None:
    assert_error(response({"ok": False, "error": "could not decode request"}))


def test_error_wrong_text() -> None:
    with pytest.raises(AssertionError, match="expected 'x', got 'y'"):
        assert_error(response({"ok": False, "error": "y"}), "x")


def test_error_on_success() -> None:
    with pytest.raises(AssertionError, match="expected ok:false"):
        assert_error(response({"ok": True}))


def test_ids_from_tree() -> None:
    resp = response({"ok": True, "result": TREE})
    for source in (resp, TREE, TREE["tree"]):
        assert workspace_ids(source) == ["w1", "w2"]
        assert session_ids(source) == ["s1", "s2", "s3"]
    assert session_ids(TREE, workspace="w2") == ["s2", "s3"]
    assert active_workspace_id(TREE) == "w2"
    assert active_session_id(TREE) == "s2"
    assert find_session(TREE, "s3") == {"id": "s3", "active": False}


def test_ids_errors() -> None:
    with pytest.raises(AssertionError, match="no workspace 'w9'"):
        session_ids(TREE, workspace="w9")
    with pytest.raises(AssertionError, match="found 0"):
        find_session(TREE, "s9")
    with pytest.raises(AssertionError, match="not a tree"):
        session_ids({"windows": []})
    with pytest.raises(AssertionError, match="one active session, found"):
        active_session_id({"workspaces": [{"id": "w", "active": True, "sessions": []}]})


def test_window_ids() -> None:
    result = {"windows": [{"id": "a", "open": True}, {"id": "b", "open": False}]}
    assert window_ids(result) == ["a", "b"]
    assert window_ids(response({"ok": True, "result": result})) == ["a", "b"]
    with pytest.raises(AssertionError, match="not a window list"):
        window_ids(TREE)
