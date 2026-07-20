import pytest

from services.version_service import VersionService


@pytest.mark.parametrize(
    ("latest", "current", "expected"),
    [
        ("v2.3.0.post1", "v2.3.0.post1", (False, False)),
        ("v2.3.0.post2", "v2.3.0.post1", (True, False)),
        ("v2.4.0", "v2.3.0.post9", (True, False)),
        ("v2.3.0.post1", "v2.4.0", (False, False)),
    ],
)
def test_post_release_version_comparison(latest: str, current: str, expected):
    assert VersionService._is_newer(latest, current) == expected


@pytest.mark.parametrize(
    ("latest", "current"),
    [
        ("v2.3.0-fork.1", "v2.3.0"),
        ("not-a-version", "v2.3.0.post1"),
        ("v2.3.0.post1", "dev"),
    ],
)
def test_malformed_versions_report_comparison_failure(latest: str, current: str):
    assert VersionService._is_newer(latest, current) == (False, True)
