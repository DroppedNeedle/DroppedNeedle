import pytest

from api.v1.schemas.settings import (
    is_musicbrainz_rate_policy_public_host,
    MusicBrainzConnectionSettings,
    _OFFICIAL_MB_RATE_LIMIT,
    _OFFICIAL_MB_CONCURRENT_SEARCHES,
)

OFFICIAL = "https://musicbrainz.org/ws/2"
MIRROR = "https://mirror.example.com/ws/2"


class TestMusicBrainzRatePolicyPublicHost:
    def test_official_https(self):
        assert (
            is_musicbrainz_rate_policy_public_host("https://musicbrainz.org/ws/2")
            is True
        )

    def test_official_http(self):
        assert (
            is_musicbrainz_rate_policy_public_host("http://musicbrainz.org/ws/2")
            is True
        )

    def test_official_www(self):
        assert (
            is_musicbrainz_rate_policy_public_host("https://www.musicbrainz.org/ws/2")
            is True
        )

    def test_official_uppercase(self):
        assert (
            is_musicbrainz_rate_policy_public_host("https://MUSICBRAINZ.ORG/ws/2")
            is True
        )

    def test_official_trailing_slash(self):
        assert (
            is_musicbrainz_rate_policy_public_host("https://musicbrainz.org/ws/2/")
            is True
        )

    def test_official_with_spaces(self):
        assert (
            is_musicbrainz_rate_policy_public_host("  https://musicbrainz.org/ws/2  ")
            is True
        )

    def test_custom_mirror(self):
        assert (
            is_musicbrainz_rate_policy_public_host("https://my-mirror.example.com/ws/2")
            is False
        )

    def test_localhost(self):
        assert (
            is_musicbrainz_rate_policy_public_host("http://localhost:5000/ws/2")
            is False
        )

    def test_empty_string(self):
        assert is_musicbrainz_rate_policy_public_host("") is False

    def test_not_a_url(self):
        assert is_musicbrainz_rate_policy_public_host("not a url") is False

    def test_subdomain_not_www(self):
        assert (
            is_musicbrainz_rate_policy_public_host("https://api.musicbrainz.org/ws/2")
            is False
        )


class TestMusicBrainzSettingsClamping:
    def test_official_url_clamps_rate_limit(self):
        settings = MusicBrainzConnectionSettings(
            api_url="https://musicbrainz.org/ws/2",
            rate_limit=10.0,
            concurrent_searches=6,
        )
        assert settings.rate_limit == _OFFICIAL_MB_RATE_LIMIT

    def test_official_url_clamps_concurrent_searches(self):
        settings = MusicBrainzConnectionSettings(
            api_url="https://musicbrainz.org/ws/2",
            rate_limit=1.0,
            concurrent_searches=20,
        )
        assert settings.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES

    def test_official_url_clamps_both(self):
        settings = MusicBrainzConnectionSettings(
            api_url="https://musicbrainz.org/ws/2",
            rate_limit=50.0,
            concurrent_searches=30,
        )
        assert settings.rate_limit == _OFFICIAL_MB_RATE_LIMIT
        assert settings.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES

    def test_official_url_does_not_increase_low_values(self):
        settings = MusicBrainzConnectionSettings(
            api_url="https://musicbrainz.org/ws/2",
            rate_limit=0.5,
            concurrent_searches=3,
        )
        assert settings.rate_limit == 0.5
        assert settings.concurrent_searches == 3

    def test_http_official_host_still_clamps_rate_and_capacity(self):
        settings = MusicBrainzConnectionSettings(
            api_url="http://musicbrainz.org:80/ws/2",
            rate_limit=50.0,
            concurrent_searches=30,
        )
        assert settings.rate_limit == _OFFICIAL_MB_RATE_LIMIT
        assert settings.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES

    def test_custom_url_allows_high_rate_limit(self):
        settings = MusicBrainzConnectionSettings(
            source_mode="mirror",
            api_url="https://my-mirror.example.com/ws/2",
            rate_limit=25.0,
            concurrent_searches=20,
        )
        assert settings.rate_limit == 25.0
        assert settings.concurrent_searches == 20

    def test_defaults_unchanged(self):
        settings = MusicBrainzConnectionSettings()
        assert settings.rate_limit == 1.0
        assert settings.concurrent_searches == 6
        assert settings.api_url == "https://musicbrainz.org/ws/2"


class TestInstanceId:
    def test_ensure_instance_id_generates_on_first_run(self, tmp_path):
        from core.config import Settings
        from services.preferences_service import PreferencesService

        config_path = tmp_path / "config.json"
        settings = Settings(config_file_path=config_path, root_app_dir=tmp_path)
        prefs = PreferencesService(settings)

        instance_id = prefs.get_instance_id()
        assert instance_id != "unknown"
        assert len(instance_id) == 36  # UUID format: 8-4-4-4-12

    def test_instance_id_is_stable_across_loads(self, tmp_path):
        from core.config import Settings
        from services.preferences_service import PreferencesService

        config_path = tmp_path / "config.json"
        settings = Settings(config_file_path=config_path, root_app_dir=tmp_path)
        prefs1 = PreferencesService(settings)
        id1 = prefs1.get_instance_id()

        prefs2 = PreferencesService(settings)
        id2 = prefs2.get_instance_id()

        assert id1 == id2

    def test_instance_id_in_user_agent(self, tmp_path):
        from core.config import Settings

        settings = Settings(
            instance_id="a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            root_app_dir=tmp_path,
        )
        ua = settings.get_user_agent()
        assert ua.startswith("DroppedNeedleApp/")
        assert "a1b2c3d4" in ua

    def test_user_agent_uses_default_contact_when_empty(self, tmp_path):
        from core.config import Settings

        settings = Settings(
            instance_id="a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            contact_email="",
            root_app_dir=tmp_path,
        )
        ua = settings.get_user_agent()
        assert "contact@droppedneedle.com" in ua
        assert "; ;" not in ua

    def test_user_agent_unknown_when_no_instance_id(self, tmp_path):
        from core.config import Settings

        settings = Settings(instance_id="", root_app_dir=tmp_path)
        ua = settings.get_user_agent()
        assert "unknown" in ua


class TestNonOfficialWidenedBounds:
    """0.1-500 r/s and 1-64 concurrent off-official; 0 = Unlimited sentinel."""

    @pytest.mark.parametrize("rate", [0.1, 1.0, 50.0, 250.5, 500.0])
    def test_accepts_widened_rate_bounds(self, rate):
        settings = MusicBrainzConnectionSettings(
            source_mode="mirror", api_url=MIRROR, rate_limit=rate, concurrent_searches=6
        )
        assert settings.rate_limit == rate
        assert settings.clamped_to_official_limits is False

    @pytest.mark.parametrize("concurrent", [1, 30, 64])
    def test_accepts_widened_concurrent_bounds(self, concurrent):
        settings = MusicBrainzConnectionSettings(
            source_mode="mirror",
            api_url=MIRROR,
            rate_limit=10.0,
            concurrent_searches=concurrent,
        )
        assert settings.concurrent_searches == concurrent

    def test_accepts_unlimited_sentinel(self):
        settings = MusicBrainzConnectionSettings(
            source_mode="mirror", api_url=MIRROR, rate_limit=0, concurrent_searches=64
        )
        assert settings.rate_limit == 0
        assert settings.clamped_to_official_limits is False

    @pytest.mark.parametrize("rate", [-0.1, 0.05, 500.1, 501.0])
    def test_rejects_out_of_bounds_rates(self, rate):
        with pytest.raises(Exception, match="rate_limit"):
            MusicBrainzConnectionSettings(
                source_mode="mirror",
                api_url=MIRROR,
                rate_limit=rate,
                concurrent_searches=6,
            )

    @pytest.mark.parametrize("concurrent", [0, 65, 100])
    def test_rejects_out_of_bounds_concurrency(self, concurrent):
        with pytest.raises(Exception, match="concurrent_searches"):
            MusicBrainzConnectionSettings(
                source_mode="mirror",
                api_url=MIRROR,
                rate_limit=10.0,
                concurrent_searches=concurrent,
            )


class TestOfficialClampWarning:
    """Official ceilings are absolute; raised entries surface
    clamped_to_official_limits on the save/settings response (applied, never
    refused). Channel choice: a field on MusicBrainzConnectionSettings itself -
    the PUT /settings/musicbrainz response already returns the mutated struct."""

    def test_official_raised_values_surface_clamp_warning(self):
        settings = MusicBrainzConnectionSettings(
            api_url=OFFICIAL, rate_limit=120.0, concurrent_searches=32
        )
        assert settings.rate_limit == _OFFICIAL_MB_RATE_LIMIT
        assert settings.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES
        assert settings.clamped_to_official_limits is True

    def test_official_sentinel_lifts_to_official_rate_and_warns(self):
        # the Unlimited sentinel is valid OFF-OFFICIAL ONLY: on the official
        # host it lifts to the official rate instead of disabling the limiter
        settings = MusicBrainzConnectionSettings(
            api_url=OFFICIAL, rate_limit=0, concurrent_searches=64
        )
        assert settings.rate_limit == _OFFICIAL_MB_RATE_LIMIT
        assert settings.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES
        assert settings.clamped_to_official_limits is True

    def test_official_at_limits_never_warns(self):
        settings = MusicBrainzConnectionSettings(
            api_url=OFFICIAL, rate_limit=1.0, concurrent_searches=6
        )
        assert settings.clamped_to_official_limits is False

    def test_official_below_limits_never_warns(self):
        settings = MusicBrainzConnectionSettings(
            api_url=OFFICIAL, rate_limit=0.5, concurrent_searches=3
        )
        assert settings.rate_limit == 0.5
        assert settings.concurrent_searches == 3
        assert settings.clamped_to_official_limits is False

    @pytest.mark.parametrize("rate,concurrent", [(500.0, 64), (0, 64)])
    def test_mirror_extremes_never_set_the_warning(self, rate, concurrent):
        settings = MusicBrainzConnectionSettings(
            source_mode="mirror",
            api_url=MIRROR,
            rate_limit=rate,
            concurrent_searches=concurrent,
        )
        assert settings.clamped_to_official_limits is False
