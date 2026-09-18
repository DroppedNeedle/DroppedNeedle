"""OpenSubsonic transcoding v1 decision and opaque stream parameters."""

from __future__ import annotations

import time

import msgspec
from api.v1.schemas.settings import ConnectAppsSettings
from core.exceptions import ValidationError
from infrastructure.crypto import decrypt, encrypt
from services.compat.transcode_service import ffmpeg_available
from services.compat.view_models import ViewTrack

_TOKEN_TTL_SECONDS = 5 * 60
_OUTPUT_FORMATS = {"mp3", "opus"}
_PROTOCOLS = {"http", "hls"}


def _bps_to_kbps(value: int) -> int:
    return (value + 500) // 1000


class _TranscodeToken(msgspec.Struct, forbid_unknown_fields=True):
    user_id: str
    file_id: str
    direct: bool
    out_format: str | None
    bitrate_kbps: int | None
    expires_at: int


class AdvancedDirectPlayProfile(msgspec.Struct, frozen=True):
    containers: tuple[str, ...]
    audio_codecs: tuple[str, ...]
    protocols: tuple[str, ...]
    max_audio_channels: int | None


class AdvancedTranscodingProfile(msgspec.Struct, frozen=True):
    container: str
    audio_codec: str
    protocol: str
    max_audio_channels: int | None


class AdvancedCodecLimitation(msgspec.Struct, frozen=True):
    name: str
    comparison: str
    values: tuple[str, ...]
    required: bool


class AdvancedCodecProfile(msgspec.Struct, frozen=True):
    profile_type: str
    name: str
    limitations: tuple[AdvancedCodecLimitation, ...]


class AdvancedClientInfo(msgspec.Struct, frozen=True):
    name: str
    platform: str
    max_audio_bitrate: int | None
    max_transcoding_audio_bitrate: int | None
    direct_play_profiles: tuple[AdvancedDirectPlayProfile, ...]
    transcoding_profiles: tuple[AdvancedTranscodingProfile, ...]
    codec_profiles: tuple[AdvancedCodecProfile, ...]


class AdvancedStreamDetails(msgspec.Struct, frozen=True):
    protocol: str
    container: str
    codec: str
    audio_channels: int | None = None
    audio_bitrate: int | None = None
    audio_samplerate: int | None = None
    audio_bitdepth: int | None = None


class AdvancedDecision(msgspec.Struct, frozen=True):
    can_direct_play: bool
    can_transcode: bool
    transcode_reason: tuple[str, ...] = ()
    error_reason: str | None = None
    transcode_params: str | None = None
    source_stream: AdvancedStreamDetails | None = None
    transcode_stream: AdvancedStreamDetails | None = None


class AdvancedTranscodeService:
    def decide(
        self,
        track: ViewTrack,
        client: AdvancedClientInfo,
        *,
        user_id: str,
        settings: ConnectAppsSettings,
    ) -> AdvancedDecision:
        self._validate_client(client)
        source_format = (track.file_format or "").lower()
        direct = any(
            self._direct_profile_matches(profile, track, source_format)
            for profile in client.direct_play_profiles
        ) and self._codec_profiles_allow(client, track, source_format)
        if (
            direct
            and client.max_audio_bitrate
            and track.bitrate is not None
            and track.bitrate * 1000 > client.max_audio_bitrate
        ):
            direct = False
        profile = next(
            (
                item
                for item in client.transcoding_profiles
                if item.protocol == "http"
                and item.audio_codec.lower() in _OUTPUT_FORMATS
                and item.container.lower() in _OUTPUT_FORMATS
                and (
                    item.max_audio_channels is None
                    or (track.channels or 2) <= item.max_audio_channels
                )
            ),
            None,
        )
        can_transcode = bool(
            profile is not None and settings.transcoding_enabled and ffmpeg_available()
        )
        source = AdvancedStreamDetails(
            protocol="http",
            container=source_format or "unknown",
            codec=source_format or "unknown",
            audio_channels=track.channels,
            audio_bitrate=track.bitrate * 1000 if track.bitrate is not None else None,
            audio_samplerate=track.sample_rate,
            audio_bitdepth=track.bit_depth,
        )
        if direct:
            token = self._encode_token(
                user_id=user_id,
                file_id=track.file_id,
                direct=True,
            )
            return AdvancedDecision(
                can_direct_play=True,
                can_transcode=False,
                transcode_params=token,
                source_stream=source,
            )
        if not can_transcode or profile is None:
            return AdvancedDecision(
                can_direct_play=direct,
                can_transcode=False,
                transcode_reason=() if direct else ("No supported transcoding profile",),
                error_reason=None if direct else "No compatible playback path",
                source_stream=source,
            )
        ceilings = [settings.transcode_max_bitrate_kbps]
        if client.max_audio_bitrate:
            ceilings.append(_bps_to_kbps(client.max_audio_bitrate))
        if client.max_transcoding_audio_bitrate:
            ceilings.append(_bps_to_kbps(client.max_transcoding_audio_bitrate))
        bitrate = max(64, min(ceilings))
        output = profile.audio_codec.lower()
        token = self._encode_token(
            user_id=user_id,
            file_id=track.file_id,
            direct=False,
            out_format=output,
            bitrate_kbps=bitrate,
        )
        return AdvancedDecision(
            can_direct_play=direct,
            can_transcode=True,
            transcode_reason=() if direct else ("Source is outside direct-play profiles",),
            transcode_params=token,
            source_stream=source,
            transcode_stream=AdvancedStreamDetails(
                protocol="http",
                container=profile.container.lower(),
                codec=output,
                audio_channels=min(track.channels or 2, profile.max_audio_channels or 2),
                audio_bitrate=bitrate * 1000,
            ),
        )

    def decode_params(
        self, value: str, *, user_id: str, file_id: str
    ) -> tuple[bool, str | None, int | None]:
        plaintext, invalid = decrypt(value)
        if invalid:
            raise ValidationError("Invalid transcode parameters")
        try:
            token = msgspec.json.decode(plaintext, type=_TranscodeToken)
        except msgspec.DecodeError as exc:
            raise ValidationError("Invalid transcode parameters") from exc
        if (
            token.user_id != user_id
            or token.file_id != file_id
            or token.expires_at < int(time.time())
            or token.direct and (token.out_format is not None or token.bitrate_kbps is not None)
            or not token.direct
            and (
                token.out_format not in _OUTPUT_FORMATS
                or token.bitrate_kbps is None
                or not 64 <= token.bitrate_kbps <= 1_000_000
            )
        ):
            raise ValidationError("Invalid transcode parameters")
        return token.direct, token.out_format, token.bitrate_kbps

    @staticmethod
    def _encode_token(
        *,
        user_id: str,
        file_id: str,
        direct: bool,
        out_format: str | None = None,
        bitrate_kbps: int | None = None,
    ) -> str:
        return encrypt(
            msgspec.json.encode(
                _TranscodeToken(
                    user_id=user_id,
                    file_id=file_id,
                    direct=direct,
                    out_format=out_format,
                    bitrate_kbps=bitrate_kbps,
                    expires_at=int(time.time()) + _TOKEN_TTL_SECONDS,
                )
            ).decode()
        )

    @staticmethod
    def _direct_profile_matches(profile, track: ViewTrack, source_format: str) -> bool:
        return (
            (not profile.containers or source_format in {v.lower() for v in profile.containers})
            and (not profile.audio_codecs or source_format in {v.lower() for v in profile.audio_codecs})
            and (not profile.protocols or "http" in profile.protocols)
            and (
                profile.max_audio_channels is None
                or (track.channels or 2) <= profile.max_audio_channels
            )
        )

    @staticmethod
    def _validate_client(client: AdvancedClientInfo) -> None:
        if not client.name or len(client.name) > 256 or not client.platform or len(client.platform) > 256:
            raise ValidationError("Invalid transcode client information")
        for value in (client.max_audio_bitrate, client.max_transcoding_audio_bitrate):
            # OpenSubsonic: 0 means "no limitation" (Feishin sends this); the
            # decision logic below already treats it as falsy/unset.
            if value not in (None, 0) and not 1 <= value <= 1_000_000_000:
                raise ValidationError("Invalid transcode bitrate")
        if len(client.direct_play_profiles) > 100 or len(client.transcoding_profiles) > 100:
            raise ValidationError("Too many transcode profiles")
        for profile in client.direct_play_profiles:
            if (
                any(protocol not in _PROTOCOLS for protocol in profile.protocols)
                or profile.max_audio_channels is not None
                and not 1 <= profile.max_audio_channels <= 128
            ):
                raise ValidationError("Invalid direct-play profile")
        for profile in client.transcoding_profiles:
            if (
                profile.protocol not in _PROTOCOLS
                or profile.max_audio_channels is not None
                and not 1 <= profile.max_audio_channels <= 128
            ):
                raise ValidationError("Invalid transcoding profile")
        allowed_names = {
            "audioChannels",
            "audioBitrate",
            "audioProfile",
            "audioSamplerate",
            "audioBitdepth",
        }
        allowed_comparisons = {
            "Equals",
            "NotEquals",
            "LessThanEqual",
            "GreaterThanEqual",
        }
        for profile in client.codec_profiles:
            if profile.profile_type != "AudioCodec" or len(profile.limitations) > 100:
                raise ValidationError("Invalid codec profile")
            for limitation in profile.limitations:
                if (
                    limitation.name not in allowed_names
                    or limitation.comparison not in allowed_comparisons
                    or not limitation.values
                    or len(limitation.values) > 100
                ):
                    raise ValidationError("Invalid codec limitation")

    @classmethod
    def _codec_profiles_allow(
        cls, client: AdvancedClientInfo, track: ViewTrack, source_format: str
    ) -> bool:
        profiles = [
            profile
            for profile in client.codec_profiles
            if profile.name.casefold() == source_format.casefold()
        ]
        return all(
            cls._limitation_allows(limitation, track)
            for profile in profiles
            for limitation in profile.limitations
            if limitation.required
        )

    @staticmethod
    def _limitation_allows(limitation: AdvancedCodecLimitation, track: ViewTrack) -> bool:
        actuals = {
            "audioChannels": track.channels,
            "audioBitrate": track.bitrate * 1000 if track.bitrate is not None else None,
            "audioSamplerate": track.sample_rate,
            "audioBitdepth": track.bit_depth,
            "audioProfile": None,
        }
        actual = actuals[limitation.name]
        if actual is None:
            return False
        values = limitation.values
        if limitation.comparison == "Equals":
            return str(actual) in values
        if limitation.comparison == "NotEquals":
            return str(actual) not in values
        try:
            expected = float(values[0])
            numeric = float(actual)
        except ValueError:
            return False
        if limitation.comparison == "LessThanEqual":
            return numeric <= expected
        return numeric >= expected
