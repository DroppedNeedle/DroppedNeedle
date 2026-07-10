import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import type { QueueItem } from '$lib/player/types';

export interface PlaylistTrack {
	id: string;
	position: number;
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id: string | null;
	artist_id: string | null;
	track_source_id: string | null;
	cover_url: string | null;
	source_type: string;
	available_sources: string[] | null;
	format: string | null;
	track_number: number | null;
	disc_number: number | null;
	duration: number | null;
	created_at: string;
	plex_rating_key: string | null;
}

export interface PlaylistSummary {
	id: string;
	name: string;
	track_count: number;
	total_duration: number | null;
	cover_urls: string[];
	custom_cover_url: string | null;
	source_ref: string | null;
	created_at: string;
	updated_at: string;
	// Ownership / visibility (D4).
	is_public: boolean;
	is_owner: boolean;
	owner_name: string | null;
	is_redacted: false;
}

export interface PlaylistDetail extends PlaylistSummary {
	tracks: PlaylistTrack[];
}

/** Admin's view of another user's PRIVATE playlist (D4): count + owner only. */
export interface RedactedPlaylist {
	id: string;
	track_count: number;
	owner_name: string | null;
	is_redacted: true;
}

export type PlaylistListItem = PlaylistSummary | RedactedPlaylist;
export type PlaylistDetailItem = PlaylistDetail | RedactedPlaylist;

export function isRedactedPlaylist(
	item: PlaylistListItem | PlaylistDetailItem
): item is RedactedPlaylist {
	return item.is_redacted === true;
}

export interface TrackData {
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id?: string | null;
	artist_id?: string | null;
	track_source_id?: string | null;
	cover_url?: string | null;
	source_type: string;
	available_sources?: string[] | null;
	format?: string | null;
	track_number?: number | null;
	disc_number?: number | null;
	duration?: number | null;
	plex_rating_key?: string | null;
}

export function queueItemToTrackData(item: QueueItem): TrackData {
	return {
		track_name: item.trackName,
		artist_name: item.artistName,
		album_name: item.albumName,
		album_id: item.albumId || null,
		artist_id: item.artistId || null,
		track_source_id: item.trackSourceId || null,
		cover_url: item.coverUrl,
		source_type: item.sourceType,
		available_sources: item.availableSources ?? null,
		format: item.format ?? null,
		track_number: item.trackNumber ?? null,
		disc_number: item.discNumber ?? null,
		duration: item.duration ?? null,
		plex_rating_key: item.plexRatingKey ?? null
	};
}

export async function fetchPlaylists(): Promise<PlaylistListItem[]> {
	const data = await api.global.get<{ playlists: PlaylistListItem[] }>(API.playlists.list());
	return data.playlists;
}

export async function fetchPlaylist(
	id: string,
	options?: { signal?: AbortSignal }
): Promise<PlaylistDetailItem> {
	return api.global.get<PlaylistDetailItem>(API.playlists.detail(id), { signal: options?.signal });
}

export async function setPlaylistPublic(id: string, isPublic: boolean): Promise<PlaylistSummary> {
	return api.global.patch<PlaylistSummary>(API.playlists.share(id), { is_public: isPublic });
}

export async function createPlaylist(name: string): Promise<PlaylistDetail> {
	return api.global.post<PlaylistDetail>(API.playlists.create(), { name });
}

export type SmartMixSeedType = 'artist' | 'genre' | 'mood';

export interface SmartMixSeed {
	type: SmartMixSeedType;
	value: string;
}

export interface GenerateSmartMixInput {
	seeds: SmartMixSeed[];
	count?: number;
	name?: string;
}

/** Smart Mix: generate and persist a playlist from a blend of artist / genre / mood seeds. */
export async function generateSmartMix(input: GenerateSmartMixInput): Promise<PlaylistDetail> {
	return api.global.post<PlaylistDetail>(API.playlists.generate(), input);
}

export async function updatePlaylist(id: string, data: { name?: string }): Promise<PlaylistDetail> {
	return api.global.put<PlaylistDetail>(API.playlists.update(id), data);
}

export async function deletePlaylist(id: string): Promise<void> {
	await api.global.delete(API.playlists.delete(id));
}

export async function addTracksToPlaylist(
	id: string,
	tracks: TrackData[],
	position?: number
): Promise<PlaylistTrack[]> {
	const body: { tracks: TrackData[]; position?: number } = { tracks };
	if (position != null) body.position = position;
	const data = await api.global.post<{ tracks: PlaylistTrack[] }>(
		API.playlists.addTracks(id),
		body
	);
	return data.tracks;
}

export async function removeTrackFromPlaylist(id: string, trackId: string): Promise<void> {
	await api.global.delete(API.playlists.removeTrack(id, trackId));
}

export async function removeTracksFromPlaylist(id: string, trackIds: string[]): Promise<void> {
	await api.global.post(API.playlists.removeTracks(id), { track_ids: trackIds });
}

export async function updatePlaylistTrack(
	id: string,
	trackId: string,
	data: { source_type?: string; available_sources?: string[] }
): Promise<PlaylistTrack> {
	return api.global.patch<PlaylistTrack>(API.playlists.updateTrack(id, trackId), data);
}

export async function reorderPlaylistTrack(
	id: string,
	trackId: string,
	newPosition: number
): Promise<{ actual_position: number }> {
	return api.global.patch<{ actual_position: number }>(API.playlists.reorderTrack(id), {
		track_id: trackId,
		new_position: newPosition
	});
}

export async function uploadPlaylistCover(id: string, file: File): Promise<{ cover_url: string }> {
	const formData = new FormData();
	formData.append('cover_image', file);
	return api.global.upload<{ cover_url: string }>(API.playlists.uploadCover(id), formData);
}

export async function deletePlaylistCover(id: string): Promise<void> {
	await api.global.delete(API.playlists.deleteCover(id));
}

export async function checkTrackMembership(
	tracks: { track_name: string; artist_name: string; album_name: string }[]
): Promise<Record<string, number[]>> {
	const data = await api.global.post<{ membership: Record<string, number[]> }>(
		API.playlists.checkTracks(),
		{ tracks }
	);
	return data.membership;
}

export async function resolvePlaylistSources(id: string): Promise<Record<string, string[]>> {
	const data = await api.global.post<{ sources: Record<string, string[]> }>(
		API.playlists.resolveSources(id)
	);
	return data.sources;
}

export interface BatchRequestResult {
	success: boolean;
	message: string;
	requested: number;
	skipped: number;
}

export async function requestMissingTracks(id: string): Promise<BatchRequestResult> {
	return api.global.post<BatchRequestResult>(API.playlists.requestMissing(id));
}
