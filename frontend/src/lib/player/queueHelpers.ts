import type {
	LocalTrackInfo,
	NativeTrackListItem,
	QueueItem,
	SourceType,
	YouTubeTrackLink
} from '$lib/player/types';
import type { RemoteTrack } from '$lib/queries/remotes/types';
import type { PlaylistTrack } from '$lib/api/playlists';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import { getCoverUrl } from '$lib/utils/errorHandling';

const CODEC_ALIASES: Record<string, string> = { alac: 'flac' };

export function normalizeCodec(codec: string | undefined | null): string {
	const raw = codec?.toLowerCase() ?? '';
	if (!raw) return 'aac';
	if (CODEC_ALIASES[raw]) return CODEC_ALIASES[raw];
	// Unknown codecs (wma included) pass through honestly: relabeling one
	// as aac would lie about what is about to play, and every surface
	// downstream renders the raw value as-is.
	return raw;
}

// Remote views carry no codec, so a remote item takes the missing-codec default.
export const REMOTE_FORMAT = normalizeCodec(null);

/** The key a remote track streams by: Plex streams a media part, the others the item. */
export function remoteStreamKey(track: RemoteTrack): string | null {
	return track.source === 'plex' ? (track.part_key ?? null) : track.id;
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
	jellyfinTrack?: RemoteTrack | null;
	navidromeTrack?: RemoteTrack | null;
	localTrack?: LocalTrackInfo | null;
	plexTrack?: RemoteTrack | null;
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
			streamUrl: gatewayStreamUrl('local', data.localTrack.track_file_id),
			format
		};
	}
	const remote = data.navidromeTrack ?? data.jellyfinTrack ?? data.plexTrack;
	if (!remote) return null;
	const key = remoteStreamKey(remote);
	if (!key) return null;
	return {
		sourceType: remote.source,
		trackSourceId: key,
		streamUrl: gatewayStreamUrl(remote.source, key),
		format: REMOTE_FORMAT
	};
}

/**
 * The recording a source gave for its file, kept under that source only so
 * switching an item to another source never carries it along.
 */
export function recordingFor(
	source: SourceType,
	recordingMbid: string | null | undefined
): Partial<Record<SourceType, string>> | undefined {
	return recordingMbid ? { [source]: recordingMbid } : undefined;
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
	if (data.navidromeTrack) sourceIds.navidrome = data.navidromeTrack.id;
	if (data.jellyfinTrack) sourceIds.jellyfin = data.jellyfinTrack.id;
	if (data.plexTrack?.part_key) sourceIds.plex = data.plexTrack.part_key;

	const recordingIds: Partial<Record<SourceType, string>> = {
		...recordingFor('navidrome', data.navidromeTrack?.recording_mbid),
		...recordingFor('jellyfin', data.jellyfinTrack?.recording_mbid),
		...(data.plexTrack?.part_key ? recordingFor('plex', data.plexTrack.recording_mbid) : {})
	};

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
		recordingIds: Object.keys(recordingIds).length > 0 ? recordingIds : undefined,
		duration: data.trackLength,
		plexRatingKey: data.plexTrack?.id
	};
}

/** Queue items for one album's remote tracks; a track that cannot stream is left out. */
export function buildRemoteQueueItems(tracks: RemoteTrack[], meta: TrackMeta): QueueItem[] {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);
	return tracks.flatMap((t) => {
		const key = remoteStreamKey(t);
		if (!key) return [];
		return [
			{
				trackSourceId: key,
				trackName: t.title,
				artistName: meta.artistName,
				trackNumber: t.track_number ?? 0,
				discNumber: normalizeDiscNumber(t.disc_number),
				albumId: meta.albumId,
				albumName: meta.albumName,
				coverUrl: normalizedCoverUrl,
				sourceType: t.source,
				artistId: meta.artistId,
				streamUrl: gatewayStreamUrl(t.source, key),
				format: REMOTE_FORMAT,
				availableSources: [t.source] as SourceType[],
				recordingIds: recordingFor(t.source, t.recording_mbid),
				duration: t.duration_secs ?? 0,
				plexRatingKey: t.source === 'plex' ? t.id : undefined
			}
		];
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
		streamUrl: gatewayStreamUrl('local', t.track_file_id),
		format: t.format.toLowerCase(),
		availableSources: ['local'] as SourceType[],
		duration: t.duration_seconds ?? undefined
	}));
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
	if (sourceType === 'local') return gatewayStreamUrl('local', trackSourceId);
	if (sourceType === 'navidrome') return gatewayStreamUrl('navidrome', trackSourceId);
	if (sourceType === 'jellyfin') return gatewayStreamUrl('jellyfin', trackSourceId);
	if (sourceType === 'plex') return gatewayStreamUrl('plex', trackSourceId);
	return undefined;
}

export function playlistTrackToQueueItem(track: PlaylistTrack): QueueItem | null {
	const availableSources: SourceType[] = track.available_sources
		? (track.available_sources as SourceType[])
		: [track.source_type as SourceType];

	// Local-first, mirroring selectBestSource: a resolved local file plays from
	// the library while sourceIds keeps the imported source for manual switch.
	// Healed/legacy linked rows may have an empty track_source_id with
	// library_file_id + available_sources ['local']; fall back to local there.
	const prefersLocal = Boolean(track.library_file_id) && availableSources.includes('local');
	if (!track.track_source_id && !(prefersLocal && track.library_file_id)) return null;
	const sourceType = (prefersLocal ? 'local' : track.source_type) as SourceType;
	const trackSourceId = prefersLocal ? track.library_file_id! : track.track_source_id!;

	const sourceIds: Partial<Record<SourceType, string>> = {};
	if (track.track_source_id) sourceIds[track.source_type as SourceType] = track.track_source_id;
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
		streamUrl: gatewayStreamUrl('local', t.id),
		format: (t.format ?? '').toLowerCase(),
		availableSources: ['local'] as SourceType[],
		duration: t.duration_seconds ?? undefined
	}));
}

/** Queue items for loose remote tracks (a list, a mix, a genre), each with its own album. */
export function buildRemoteDiscoveryQueue(tracks: RemoteTrack[]): QueueItem[] {
	return tracks.flatMap((t) => {
		const key = remoteStreamKey(t);
		if (!key) return [];
		return [
			{
				trackSourceId: key,
				trackName: t.title,
				artistName: t.artist_name,
				trackNumber: t.track_number ?? 0,
				discNumber: normalizeDiscNumber(t.disc_number),
				albumId: t.source === 'jellyfin' ? (t.album_id ?? '') : '',
				albumName: t.album_name,
				coverUrl: t.image_url ?? null,
				sourceType: t.source,
				streamUrl: gatewayStreamUrl(t.source, key),
				format: REMOTE_FORMAT,
				availableSources: [t.source] as SourceType[],
				recordingIds: recordingFor(t.source, t.recording_mbid),
				duration: t.duration_secs ?? 0
			}
		];
	});
}
