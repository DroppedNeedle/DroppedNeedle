import type { QueueItem, SourceType } from '$lib/player/types';
import type {
	JellyfinTrackInfo,
	LocalTrackInfo,
	NativeTrackListItem,
	NavidromeTrackInfo,
	PlexTrackInfo,
	YouTubeTrackLink
} from '$lib/types';
import type { PlaylistTrack } from '$lib/api/playlists';
import { API } from '$lib/constants';
import { getCoverUrl } from '$lib/utils/errorHandling';

const SUPPORTED_CODECS = new Set(['aac', 'mp3', 'opus', 'flac', 'wav', 'wma', 'vorbis', 'alac']);
const CODEC_ALIASES: Record<string, string> = { alac: 'flac', wma: 'aac' };

export function normalizeCodec(codec: string | undefined | null): string {
	const raw = codec?.toLowerCase() ?? 'aac';
	if (CODEC_ALIASES[raw]) return CODEC_ALIASES[raw];
	if (SUPPORTED_CODECS.has(raw)) return raw;
	return 'aac';
}

export interface TrackMeta {
	albumId: string;
	albumName: string;
	artistName: string;
	coverUrl: string | null;
	artistId?: string;
}

export interface TrackSourceData {
	trackPosition: number;
	discNumber?: number;
	trackTitle: string;
	trackLength?: number;
	jellyfinTrack?: JellyfinTrackInfo | null;
	navidromeTrack?: NavidromeTrackInfo | null;
	localTrack?: LocalTrackInfo | null;
	plexTrack?: PlexTrackInfo | null;
}

export function normalizeDiscNumber(discNumber: number | null | undefined): number {
	return Number.isFinite(Number(discNumber)) && Number(discNumber) > 0 ? Number(discNumber) : 1;
}

export function getDiscTrackKey(track: {
	disc_number?: number | null;
	track_number?: number | null;
	position?: number | null;
}): string {
	const discNumber = normalizeDiscNumber(track.disc_number);
	const trackNumber = Number(track.track_number ?? track.position ?? 0);
	return `${discNumber}:${trackNumber}`;
}

export function compareDiscTrack(
	a: { disc_number?: number | null; track_number?: number | null; position?: number | null },
	b: { disc_number?: number | null; track_number?: number | null; position?: number | null }
): number {
	const discDiff = normalizeDiscNumber(a.disc_number) - normalizeDiscNumber(b.disc_number);
	if (discDiff !== 0) return discDiff;
	return Number(a.track_number ?? a.position ?? 0) - Number(b.track_number ?? b.position ?? 0);
}

export function selectBestSource(
	data: TrackSourceData
): { sourceType: SourceType; trackSourceId: string; streamUrl: string; format?: string } | null {
	if (data.localTrack) {
		const format = data.localTrack.format.toLowerCase();
		return {
			sourceType: 'local',
			trackSourceId: String(data.localTrack.track_file_id),
			streamUrl: API.stream.local(data.localTrack.track_file_id),
			format
		};
	}
	if (data.navidromeTrack) {
		const format = normalizeCodec(data.navidromeTrack.codec);
		return {
			sourceType: 'navidrome',
			trackSourceId: data.navidromeTrack.navidrome_id,
			streamUrl: API.stream.navidrome(data.navidromeTrack.navidrome_id),
			format
		};
	}
	if (data.jellyfinTrack) {
		const format = normalizeCodec(data.jellyfinTrack.codec);
		return {
			sourceType: 'jellyfin',
			trackSourceId: data.jellyfinTrack.jellyfin_id,
			streamUrl: API.stream.jellyfin(data.jellyfinTrack.jellyfin_id),
			format
		};
	}
	if (data.plexTrack) {
		if (!data.plexTrack.part_key) return null;
		const format = normalizeCodec(data.plexTrack.codec);
		return {
			sourceType: 'plex',
			trackSourceId: data.plexTrack.part_key,
			streamUrl: API.stream.plex(data.plexTrack.part_key),
			format
		};
	}
	return null;
}

export function getAvailableSources(data: TrackSourceData): SourceType[] {
	const sources: SourceType[] = [];
	if (data.localTrack) sources.push('local');
	if (data.navidromeTrack) sources.push('navidrome');
	if (data.jellyfinTrack) sources.push('jellyfin');
	if (data.plexTrack?.part_key) sources.push('plex');
	return sources;
}

export function buildQueueItem(meta: TrackMeta, data: TrackSourceData): QueueItem | null {
	const best = selectBestSource(data);
	if (!best) return null;

	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const sourceIds: Partial<Record<import('$lib/player/types').SourceType, string>> = {};
	if (data.localTrack) sourceIds.local = String(data.localTrack.track_file_id);
	if (data.navidromeTrack) sourceIds.navidrome = data.navidromeTrack.navidrome_id;
	if (data.jellyfinTrack) sourceIds.jellyfin = data.jellyfinTrack.jellyfin_id;
	if (data.plexTrack?.part_key) sourceIds.plex = data.plexTrack.part_key;

	return {
		trackSourceId: best.trackSourceId,
		trackName: data.trackTitle,
		artistName: meta.artistName,
		trackNumber: data.trackPosition,
		discNumber: data.discNumber ?? 1,
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		sourceType: best.sourceType,
		artistId: meta.artistId,
		streamUrl: best.streamUrl,
		format: best.format,
		availableSources: getAvailableSources(data),
		sourceIds,
		duration: data.trackLength,
		plexRatingKey: data.plexTrack?.plex_id
	};
}

export function buildQueueItemsFromJellyfin(
	tracks: JellyfinTrackInfo[],
	meta: TrackMeta
): QueueItem[] {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);
	return tracks.map((t) => {
		const format = normalizeCodec(t.codec);
		return {
			trackSourceId: t.jellyfin_id,
			trackName: t.title,
			artistName: meta.artistName,
			trackNumber: t.track_number,
			discNumber: normalizeDiscNumber(t.disc_number),
			albumId: meta.albumId,
			albumName: meta.albumName,
			coverUrl: normalizedCoverUrl,
			sourceType: 'jellyfin' as const,
			artistId: meta.artistId,
			streamUrl: API.stream.jellyfin(t.jellyfin_id),
			format,
			availableSources: ['jellyfin'] as SourceType[],
			duration: t.duration_seconds
		};
	});
}

export function buildQueueItemsFromNavidrome(
	tracks: NavidromeTrackInfo[],
	meta: TrackMeta
): QueueItem[] {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);
	return tracks.map((t) => {
		const format = normalizeCodec(t.codec);
		return {
			trackSourceId: t.navidrome_id,
			trackName: t.title,
			artistName: meta.artistName,
			trackNumber: t.track_number,
			discNumber: normalizeDiscNumber(t.disc_number),
			albumId: meta.albumId,
			albumName: meta.albumName,
			coverUrl: normalizedCoverUrl,
			sourceType: 'navidrome' as const,
			artistId: meta.artistId,
			streamUrl: API.stream.navidrome(t.navidrome_id),
			format,
			availableSources: ['navidrome'] as SourceType[],
			duration: t.duration_seconds
		};
	});
}

export function buildQueueItemsFromLocal(tracks: LocalTrackInfo[], meta: TrackMeta): QueueItem[] {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);
	return tracks.map((t) => ({
		trackSourceId: String(t.track_file_id),
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number,
		discNumber: normalizeDiscNumber(t.disc_number),
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		coverRemoteUrl: meta.coverUrl?.startsWith('http') ? meta.coverUrl : null,
		sourceType: 'local' as const,
		artistId: meta.artistId,
		streamUrl: API.stream.local(t.track_file_id),
		format: t.format.toLowerCase(),
		availableSources: ['local'] as SourceType[],
		duration: t.duration_seconds ?? undefined
	}));
}

export function buildQueueItemsFromPlex(tracks: PlexTrackInfo[], meta: TrackMeta): QueueItem[] {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);
	return tracks
		.filter((t) => t.part_key)
		.map((t) => {
			const format = normalizeCodec(t.codec);
			return {
				trackSourceId: t.part_key!,
				trackName: t.title,
				artistName: meta.artistName,
				trackNumber: t.track_number,
				discNumber: normalizeDiscNumber(t.disc_number),
				albumId: meta.albumId,
				albumName: meta.albumName,
				coverUrl: normalizedCoverUrl,
				sourceType: 'plex' as const,
				artistId: meta.artistId,
				streamUrl: API.stream.plex(t.part_key!),
				format,
				availableSources: ['plex'] as SourceType[],
				duration: t.duration_seconds,
				plexRatingKey: t.plex_id
			};
		});
}

export function buildQueueItemFromYouTube(track: YouTubeTrackLink, meta: TrackMeta): QueueItem {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);
	return {
		trackSourceId: track.video_id,
		trackName: track.track_name,
		artistName: meta.artistName,
		trackNumber: track.track_number,
		discNumber: normalizeDiscNumber(track.disc_number),
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		sourceType: 'youtube' as const,
		artistId: meta.artistId,
		availableSources: ['youtube'] as SourceType[]
	};
}

export function buildQueueItemsFromYouTube(
	tracks: YouTubeTrackLink[],
	meta: TrackMeta
): QueueItem[] {
	return tracks.map((t) => buildQueueItemFromYouTube(t, meta));
}

function resolveStreamUrl(sourceType: string, trackSourceId: string): string | undefined {
	if (sourceType === 'local') return API.stream.local(trackSourceId);
	if (sourceType === 'navidrome') return API.stream.navidrome(trackSourceId);
	if (sourceType === 'jellyfin') return API.stream.jellyfin(trackSourceId);
	if (sourceType === 'plex') return API.stream.plex(trackSourceId);
	return undefined;
}

export function playlistTrackToQueueItem(track: PlaylistTrack): QueueItem | null {
	if (!track.track_source_id) return null;

	const availableSources: SourceType[] = track.available_sources
		? (track.available_sources as SourceType[])
		: [track.source_type as SourceType];

	// Local-first, mirroring selectBestSource: a resolved local file plays from
	// the library while sourceIds keeps the imported source for manual switch.
	const prefersLocal = Boolean(track.library_file_id) && availableSources.includes('local');
	const sourceType = (prefersLocal ? 'local' : track.source_type) as SourceType;
	const trackSourceId = prefersLocal ? track.library_file_id! : track.track_source_id;

	const sourceIds: Partial<Record<SourceType, string>> = {
		[track.source_type as SourceType]: track.track_source_id
	};
	if (track.library_file_id) sourceIds.local = track.library_file_id;

	return {
		trackSourceId,
		trackName: track.track_name,
		artistName: track.artist_name,
		trackNumber: track.track_number ?? track.position,
		discNumber: track.disc_number ?? 1,
		albumId: track.album_id ?? '',
		albumName: track.album_name,
		coverUrl: track.cover_url,
		sourceType,
		artistId: track.artist_id ?? undefined,
		streamUrl: resolveStreamUrl(sourceType, trackSourceId),
		format: track.format ?? undefined,
		availableSources,
		sourceIds,
		duration: track.duration ?? undefined,
		playlistTrackId: track.id,
		plexRatingKey: track.source_type === 'plex' ? (track.plex_rating_key ?? undefined) : undefined
	};
}

export function buildDiscoveryQueueFromNavidrome(tracks: NavidromeTrackInfo[]): QueueItem[] {
	return tracks.map((t) => ({
		trackSourceId: t.navidrome_id,
		trackName: t.title,
		artistName: t.artist_name,
		trackNumber: t.track_number,
		discNumber: normalizeDiscNumber(t.disc_number),
		albumId: '',
		albumName: t.album_name,
		coverUrl: t.image_url ?? null,
		sourceType: 'navidrome' as const,
		streamUrl: API.stream.navidrome(t.navidrome_id),
		format: normalizeCodec(t.codec),
		availableSources: ['navidrome'] as SourceType[],
		duration: t.duration_seconds
	}));
}

export function buildDiscoveryQueueFromLocal(tracks: NativeTrackListItem[]): QueueItem[] {
	return tracks.map((t) => ({
		trackSourceId: t.id,
		trackName: t.title,
		artistName: t.artist_name,
		trackNumber: t.track_number,
		discNumber: normalizeDiscNumber(t.disc_number),
		albumId: t.album_id,
		albumName: t.album_title,
		coverUrl: getCoverUrl(null, t.album_id),
		coverRemoteUrl: null,
		sourceType: 'local' as const,
		artistId: t.artist_id,
		streamUrl: API.stream.local(t.id),
		format: (t.format ?? '').toLowerCase(),
		availableSources: ['local'] as SourceType[],
		duration: t.duration_seconds ?? undefined
	}));
}

export function buildDiscoveryQueueFromJellyfin(tracks: JellyfinTrackInfo[]): QueueItem[] {
	return tracks.map((t) => ({
		trackSourceId: t.jellyfin_id,
		trackName: t.title,
		artistName: t.artist_name,
		trackNumber: t.track_number,
		discNumber: normalizeDiscNumber(t.disc_number),
		albumId: t.album_id ?? '',
		albumName: t.album_name,
		coverUrl: t.image_url ?? null,
		sourceType: 'jellyfin' as const,
		streamUrl: API.stream.jellyfin(t.jellyfin_id),
		format: normalizeCodec(t.codec),
		availableSources: ['jellyfin'] as SourceType[],
		duration: t.duration_seconds
	}));
}

export function buildDiscoveryQueueFromPlex(tracks: PlexTrackInfo[]): QueueItem[] {
	return tracks
		.filter((t) => t.part_key)
		.map((t) => ({
			trackSourceId: t.part_key!,
			trackName: t.title,
			artistName: t.artist_name,
			trackNumber: t.track_number,
			discNumber: normalizeDiscNumber(t.disc_number),
			albumId: '',
			albumName: t.album_name,
			coverUrl: t.image_url ?? null,
			sourceType: 'plex' as const,
			streamUrl: API.stream.plex(t.part_key!),
			format: normalizeCodec(t.codec),
			availableSources: ['plex'] as SourceType[],
			duration: t.duration_seconds
		}));
}
