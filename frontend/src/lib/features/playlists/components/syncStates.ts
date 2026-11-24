import { ApiError } from '$lib/api/errors';
import type {
	PlaylistSyncV3,
	SyncFailureV3
} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';

/** How a sync's line reads: done, still going, or needs attention. */
export type SyncTone = 'ok' | 'wait' | 'warn';

/** The one line the sync band shows, and whether it points to the Profile. */
export interface SyncLine {
	text: string;
	tone: SyncTone;
	profile: boolean;
}

/** The fixed sentence for each reason a pass failed. */
export const SYNC_FAILURES: Record<SyncFailureV3, string> = {
	not_linked: "Spotify isn't connected. Connect it on your Profile.",
	needs_reconnect: 'Reconnect Spotify on your Profile to sync Liked Songs.',
	playlist_gone: 'This playlist is no longer on your Spotify account.',
	unavailable: "Spotify couldn't be reached. Trying again later.",
	rate_limited: 'Spotify asked to slow down. Trying again later.',
	save_failed: "New songs couldn't be saved. Trying again later."
};

/** The same failures as an editor reads them: the link is the owner's. */
const EDITOR_FAILURES: Partial<Record<SyncFailureV3, string>> = {
	not_linked: "The owner's Spotify isn't connected.",
	needs_reconnect: 'The owner needs to reconnect Spotify.'
};

/** The fixed sentence for each refusal the switch or a kept album can meet. */
const REFUSALS: Record<string, string> = {
	SPOTIFY_NOT_LINKED: "Spotify isn't connected. Connect it on your Profile.",
	SPOTIFY_RECONNECT_NEEDED: 'Reconnect Spotify on your Profile to sync Liked Songs.',
	SPOTIFY_OFF: 'Spotify is turned off on this server.',
	PLAYLIST_NOT_SYNCABLE: "This playlist didn't come from Spotify.",
	ALBUM_NOT_MOVED: "This song is already on Spotify's album.",
	ENTRY_CHANGED: 'This song changed since, so it stays as it is.'
};

/** What to tell a person whose sync change or kept album was refused. */
export function refusalText(error: unknown, fallback: string): string {
	return (error instanceof ApiError && REFUSALS[error.code]) || fallback;
}

/** The hour and minute a pass finished, in the viewer's time. */
function checkedAt(seconds: number): string {
	return new Date(seconds * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/**
 * What the band says about a sync, most pressing first: Spotify off on the
 * server, a failed pass, requests an admin has not granted, a full quota or
 * storage, then how far it has got. Nothing while sync is off.
 */
export function syncLine(sync: PlaylistSyncV3, isOwner = true): SyncLine | null {
	const line = (text: string, tone: SyncTone, profile = false): SyncLine => ({
		text,
		tone,
		profile
	});
	if (sync.spotify_off) return line('Spotify is turned off on this server.', 'warn');
	if (!sync.enabled) return null;
	if (sync.failure) {
		const editor = isOwner ? undefined : EDITOR_FAILURES[sync.failure];
		if (editor) return line(editor, 'warn');
		const profile = sync.failure === 'not_linked' || sync.failure === 'needs_reconnect';
		return line(SYNC_FAILURES[sync.failure], 'warn', profile);
	}
	switch (sync.grant) {
		case 'waiting':
			return line('Waiting for an admin to approve requests. New songs are still added.', 'wait');
		case 'rejected':
			return line(
				'An admin turned down requests for this playlist. New songs are still added.',
				'warn'
			);
		case 'revoked':
			return line(
				'An admin took back requests for this playlist. New songs are still added.',
				'warn'
			);
	}
	if (sync.hold === 'quota') {
		return line('Your request limit is full. Songs wait until there is room.', 'warn');
	}
	if (sync.hold === 'storage') {
		return line('Storage is full. Songs wait until there is room.', 'warn');
	}
	switch (sync.progress) {
		case 'starting':
			return line('Checking Spotify...', 'wait');
		case 'catching_up':
			return line(`Requesting missing songs: ${sync.songs_left} left.`, 'wait');
		default:
			return line(
				sync.checked_at ? `Up to date. Checked ${checkedAt(sync.checked_at)}.` : 'Up to date.',
				'ok'
			);
	}
}
