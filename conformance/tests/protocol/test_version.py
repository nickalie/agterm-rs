import pytest
from agterm_conformance.match import assert_ok, assert_version_result


@pytest.mark.covers("version", "field:result.app")
def test_version_reports_the_app_version(ctl):
    assert_version_result(ctl.request("version"))


@pytest.mark.covers("version", "field:result.app", "field:tree.app")
def test_tree_app_is_the_version_identity(ctl):
    assert ctl.tree()["tree"]["app"] == assert_ok(ctl.request("version"))["app"]
