import pytest
from agterm_conformance.match import ANY, assert_ok, assert_subset


@pytest.mark.covers("version", "field:result.app")
def test_version_reports_the_app_version(ctl):
    result = assert_ok(ctl.request("version"))
    assert_subset(result, {"app": {"version": ANY}})
    assert isinstance(result["app"]["version"], str) and result["app"]["version"]


@pytest.mark.covers("version", "field:result.app", "field:tree.app")
def test_tree_app_is_the_version_identity(ctl):
    assert ctl.tree()["tree"]["app"] == assert_ok(ctl.request("version"))["app"]
