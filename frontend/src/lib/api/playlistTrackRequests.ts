import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import type {
	BatchRequestResult,
	CompilationPolicy,
	HomeOption,
	PlaylistTrack,
	PlaylistTrackRequestSettings,
	TrackRequestJob,
	TrackRequestMode
} from '$lib/api/playlists';

// "Request all tracks" (#204). Kept apart from playlists.ts so existing
// module mocks of that file stay complete.

export interface TrackBatchRequestResult extends BatchRequestResult {
	overflow?: number;
	status?: string;
}

export async function getTrackRequestSettings(): Promise<PlaylistTrackRequestSettings> {
	return api.global.get<PlaylistTrackRequestSettings>(API.playlists.trackRequestSettings());
}

/** Work out which album each track would come from (Review mode, step 1). */
export async function resolvePlaylistTracks(
	id: string,
	compilationPolicy?: CompilationPolicy
): Promise<TrackRequestJob> {
	return api.global.post<TrackRequestJob>(API.playlists.resolveTracks(id), {
		compilation_policy: compilationPolicy ?? null
	});
}

/** Request every missing track individually; ``review`` sends what was resolved. */
export async function requestPlaylistTracks(
	id: string,
	mode?: TrackRequestMode,
	compilationPolicy?: CompilationPolicy
): Promise<TrackRequestJob> {
	return api.global.post<TrackRequestJob>(API.playlists.requestTracks(id), {
		mode: mode ?? null,
		compilation_policy: compilationPolicy ?? null
	});
}

export async function chooseTrackHome(
	id: string,
	trackId: string,
	option: Pick<HomeOption, 'recording_mbid' | 'release_group_mbid'>
): Promise<PlaylistTrack> {
	return api.global.post<PlaylistTrack>(API.playlists.chooseTrackHome(id, trackId), {
		recording_mbid: option.recording_mbid,
		release_group_mbid: option.release_group_mbid
	});
}
