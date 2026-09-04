from services.version_service import legacy_image_notice

EXPECTED_MESSAGE = (
    "legacy image names retired: installs still on habirabbu/* stop receiving updates "
    "after v2.11.x - if this is you, switch the compose image to "
    "droppedneedle/droppedneedle (or ghcr.io/droppedneedle/droppedneedle), then pull "
    "and restart"
)


def test_legacy_image_notice_returns_message_for_v211_variants():
    assert legacy_image_notice("v2.11.0") == EXPECTED_MESSAGE
    assert legacy_image_notice("2.11.0") == EXPECTED_MESSAGE
    assert legacy_image_notice("v2.11.1") == EXPECTED_MESSAGE


def test_legacy_image_notice_returns_none_for_other_versions():
    assert legacy_image_notice("v2.10.2") is None
    assert legacy_image_notice("dev") is None
    assert legacy_image_notice("hosting-local") is None
    assert legacy_image_notice("") is None
    assert legacy_image_notice("v2.12.0") is None
