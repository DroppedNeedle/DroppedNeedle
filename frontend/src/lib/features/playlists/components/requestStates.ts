import type {
	ImportFailureReasonV3,
	PlaylistEntryStateV3,
	PlaylistRequestStateV3
} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
import type { PlaylistRequestResultV3 } from '$lib/features/playlists/queries/PlaylistV3Mutations.svelte';

type Tone = 'neutral' | 'info' | 'success' | 'warning' | 'error';

/** The chip each missing song's state shows; a requestable song shows none. */
export const ENTRY_CHIPS: Record<PlaylistEntryStateV3, { label: string; tone: Tone } | null> = {
	requestable: null,
	no_album_yet: { label: 'No album found yet', tone: 'neutral' },
	finding_album: { label: 'Finding album', tone: 'neutral' },
	awaiting_approval: { label: 'Awaiting approval', tone: 'warning' },
	downloading: { label: 'Downloading', tone: 'info' },
	awaiting_review: { label: 'Awaiting review', tone: 'warning' },
	album_in_library: { label: 'Album in library', tone: 'success' },
	rejected: { label: 'Rejected', tone: 'error' },
	queued: { label: 'Queued', tone: 'neutral' }
};

/** The one line a failed Spotify import shows, by why it failed. */
export const IMPORT_FAILURES: Record<ImportFailureReasonV3, string> = {
	not_linked: 'The Spotify import failed: link Spotify again in your profile.',
	unavailable: "The Spotify import failed: Spotify didn't answer.",
	rate_limited: 'The Spotify import failed: Spotify is limiting requests.',
	save_failed: "The Spotify import failed: the playlist couldn't be saved.",
	other: "The Spotify import didn't finish."
};

/** The line the Spotify import shows while it runs or after it failed. */
export function importLine(state: PlaylistRequestStateV3 | undefined): string | null {
	const job = state?.import;
	if (!job) return null;
	return job.state === 'running' ? 'Importing from Spotify...' : IMPORT_FAILURES[job.reason];
}

const albums = (n: number) => `${n} ${n === 1 ? 'album' : 'albums'}`;

/** What a press filed and why it left the rest out, shortly. */
export function pressMessage(result: PlaylistRequestResultV3): string {
	const asked =
		result.requested > 0
			? `Asked for ${albums(result.requested)}${result.needs_approval ? ', waiting for approval' : ''}.`
			: 'Nothing new to ask for.';
	const left = [
		[result.in_library, 'in library'],
		[result.already_requested, 'already asked for'],
		[result.rejected_before, 'rejected before'],
		[result.over_cap, 'over the 200 limit'],
		[result.over_quota, 'over your quota']
	] as const;
	const skipped = left.filter(([n]) => n > 0).map(([n, why]) => `${n} ${why}`);
	return skipped.length > 0 ? `${asked} Left out: ${skipped.join(', ')}.` : asked;
}
