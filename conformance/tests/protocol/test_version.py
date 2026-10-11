import pytest
from agterm_conformance.match import ANY, assert_ok, assert_subset


@pytest.mark.covers("version")
def test_version_reports_the_app_version(ctl):
    result = assert_ok(ctl.request("version"))
    assert_subset(result, {"app": {"version": ANY}})
    assert isinstance(result["app"]["version"], str) and result["app"]["version"]
