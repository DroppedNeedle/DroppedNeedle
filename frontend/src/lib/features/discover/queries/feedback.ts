import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

export type FeedbackKind = components['schemas']['FeedbackKind'];
export type FeedbackTarget = components['schemas']['FeedbackTarget'];
export type DiscoverFeedbackEntry = components['schemas']['DiscoverFeedbackEntry'];

/** The album a person is telling Discover about, and the artist it is filed under. */
export interface FeedbackItem {
	releaseGroupMbid: string;
	artistMbid: string;
	releaseName: string;
	artistName: string;
	/** The surface the answer is given on. */
	madeFrom: components['schemas']['MadeFrom'];
}

/** Tells Discover one thing about an album, or about its artist for `not_artist`. */
export function tellDiscover(kind: FeedbackKind, item: FeedbackItem) {
	return api.v3.POST(v3('/api/v3/discover/feedback'), {
		kind,
		release_group_mbid: item.releaseGroupMbid,
		artist_mbid: item.artistMbid || undefined,
		release_name: item.releaseName,
		artist_name: item.artistName,
		made_from: item.madeFrom
	});
}

/** Takes back one thing the person told Discover. */
export function undoDiscoverFeedback(target: FeedbackTarget, mbid: string) {
	return api.v3.DELETE(v3('/api/v3/discover/feedback/{target}/{mbid}', { path: { target, mbid } }));
}
