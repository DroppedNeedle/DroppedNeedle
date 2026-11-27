import type { PlaylistSummary } from '$lib/api/playlists';

type PlaylistKind = PlaylistSummary['kind'];

/**
 * The kinds the app makes and keeps fresh for a person, in the order the
 * Made for you shelf shows them. A new kind the app makes (Daily Mixes)
 * joins this list and gets its line in the shelf's `freshness`.
 */
export const MADE_FOR_YOU_KINDS = ['weekly_mix', 'rotation'] as const satisfies PlaylistKind[];

export type MadeForYouKind = (typeof MADE_FOR_YOU_KINDS)[number];

/** True for a list of the person's own that the app makes for them. */
export function isMadeForYou(
	playlist: PlaylistSummary
): playlist is PlaylistSummary & { kind: MadeForYouKind } {
	return (
		playlist.is_owner && (MADE_FOR_YOU_KINDS as readonly PlaylistKind[]).includes(playlist.kind)
	);
}
