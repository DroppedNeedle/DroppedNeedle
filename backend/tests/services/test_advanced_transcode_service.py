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


def _client(*, samplerate_limit: str = "96000") -> AdvancedClientInfo:
    return AdvancedClientInfo(
        "client",
        "linux",
        1_000_000,
        192_000,
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


def test_zero_bitrate_means_no_limitation_per_opensubsonic_spec(tmp_path, monkeypatch):
    """Regression for #464: Feishin (and the OpenSubsonic spec) send 0 for
    maxAudioBitrate/maxTranscodingAudioBitrate to mean "no limitation", not "zero
    bitrate". The client must not be rejected before the server even evaluates
    whether the track could be direct-played."""
    init_crypto(tmp_path / "config")
    monkeypatch.setattr(
        "services.compat.advanced_transcode_service.ffmpeg_available", lambda: True
    )
    client = AdvancedClientInfo(
        "client",
        "linux",
        0,
        0,
        (AdvancedDirectPlayProfile(("flac",), ("flac",), ("http",), 2),),
        (AdvancedTranscodingProfile("mp3", "mp3", "http", 2),),
        (),
    )

    decision = AdvancedTranscodeService().decide(
        _track(),
        client,
        user_id="alice",
        settings=ConnectAppsSettings(transcoding_enabled=True),
    )

    assert decision.can_direct_play is True


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
