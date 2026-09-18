from api.v1.schemas.settings import ConnectAppsSettings
from infrastructure.crypto import init_crypto
from services.compat.advanced_transcode_service import (
    AdvancedClientInfo,
    AdvancedCodecLimitation,
    AdvancedCodecProfile,
    AdvancedDirectPlayProfile,
    AdvancedTranscodeService,
    AdvancedTranscodingProfile,
)
from services.compat.view_models import ViewTrack


def _track() -> ViewTrack:
    return ViewTrack(
        file_id="track-1",
        title="Song",
        album_title="Album",
        file_format="flac",
        bitrate=900,
        sample_rate=96_000,
        bit_depth=24,
        channels=2,
        duration_seconds=300,
    )


def _client(
    *,
    samplerate_limit: str = "96000",
    max_audio_bitrate: int | None = 1_000_000,
    max_transcoding_audio_bitrate: int | None = 192_000,
) -> AdvancedClientInfo:
    return AdvancedClientInfo(
        "client",
        "linux",
        max_audio_bitrate,
        max_transcoding_audio_bitrate,
        (AdvancedDirectPlayProfile(("flac",), ("flac",), ("http",), 2),),
        (AdvancedTranscodingProfile("mp3", "mp3", "http", 2),),
        (
            AdvancedCodecProfile(
                "AudioCodec",
                "flac",
                (
                    AdvancedCodecLimitation(
                        "audioSamplerate", "LessThanEqual", (samplerate_limit,), True
                    ),
                ),
            ),
        ),
    )


def test_zero_bitrate_means_no_limitation(tmp_path, monkeypatch):
    """A client sending 0 for the bitrate caps (OpenSubsonic "no limitation",
    e.g. Feishin with transcoding disabled) must not be rejected (#464)."""
    init_crypto(tmp_path / "config")
    monkeypatch.setattr(
        "services.compat.advanced_transcode_service.ffmpeg_available", lambda: True
    )
    decision = AdvancedTranscodeService().decide(
        _track(),
        _client(max_audio_bitrate=0, max_transcoding_audio_bitrate=0),
        user_id="alice",
        settings=ConnectAppsSettings(transcoding_enabled=True),
    )

    assert decision.can_direct_play is True


def test_zero_bitrate_falls_back_to_server_transcode_ceiling(tmp_path, monkeypatch):
    """With 0 caps and direct play unavailable, the server ceiling (not a
    zero-derived floor artifact) bounds the transcode (#464)."""
    init_crypto(tmp_path / "config")
    monkeypatch.setattr(
        "services.compat.advanced_transcode_service.ffmpeg_available", lambda: True
    )
    decision = AdvancedTranscodeService().decide(
        _track(),
        _client(
            samplerate_limit="48000",
            max_audio_bitrate=0,
            max_transcoding_audio_bitrate=0,
        ),
        user_id="alice",
        settings=ConnectAppsSettings(transcoding_enabled=True),
    )

    assert decision.can_direct_play is False
    assert decision.can_transcode is True
    assert decision.transcode_stream is not None
    assert decision.transcode_stream.audio_bitrate == 320_000


def test_advanced_decision_applies_profiles_and_scoped_opaque_token(tmp_path, monkeypatch):
    init_crypto(tmp_path / "config")
    monkeypatch.setattr(
        "services.compat.advanced_transcode_service.ffmpeg_available", lambda: True
    )
    service = AdvancedTranscodeService()
    decision = service.decide(
        _track(),
        _client(),
        user_id="alice",
        settings=ConnectAppsSettings(
            transcoding_enabled=True, transcode_max_bitrate_kbps=256
        ),
    )

    assert decision.can_direct_play is True
    assert decision.can_transcode is False
    assert decision.source_stream is not None
    assert decision.source_stream.audio_bitrate == 900_000
    assert decision.transcode_stream is None
    assert decision.transcode_params is not None
    assert service.decode_params(
        decision.transcode_params, user_id="alice", file_id="track-1"
    ) == (True, None, None)


def test_required_codec_limitation_disables_direct_play(tmp_path, monkeypatch):
    init_crypto(tmp_path / "config")
    monkeypatch.setattr(
        "services.compat.advanced_transcode_service.ffmpeg_available", lambda: True
    )
    decision = AdvancedTranscodeService().decide(
        _track(),
        _client(samplerate_limit="48000"),
        user_id="alice",
        settings=ConnectAppsSettings(transcoding_enabled=True),
    )

    assert decision.can_direct_play is False
    assert decision.can_transcode is True
    assert decision.transcode_stream is not None
    assert decision.transcode_stream.audio_bitrate == 192_000
    assert decision.transcode_params is not None
    assert AdvancedTranscodeService().decode_params(
        decision.transcode_params, user_id="alice", file_id="track-1"
    ) == (False, "mp3", 192)
