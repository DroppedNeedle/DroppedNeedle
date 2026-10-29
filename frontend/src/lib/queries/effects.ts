import type { QueryKey } from '@tanstack/svelte-query';

import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { everyScope, key, userScope } from '$lib/queries/keys';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { FollowQueryKeyFactory } from '$lib/queries/following/FollowQueryKeyFactory';
import { GenreQueryKeyFactory } from '$lib/queries/genre/GenreQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { DropImportQueryKeyFactory } from '$lib/queries/import/DropImportQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { LOCAL_KEYS } from '$lib/queries/local/LocalV3Keys';
import { LyricsQueryKeyFactory } from '$lib/queries/lyrics/LyricsQueryKeyFactory';
import { PlaylistQueryKeyFactory } from '$lib/queries/playlists/PlaylistQueryKeyFactory';
import { RatingQueryKeyFactory } from '$lib/queries/ratings/RatingQueryKeyFactory';
import { RequestQueryKeyFactory } from '$lib/queries/requests/RequestQueryKeyFactory';
import { SearchQueryKeyFactory } from '$lib/queries/search/SearchQueryKeyFactory';
import { WantedQueryKeyFactory } from '$lib/queries/wanted/WantedQueryKeyFactory';

type UserId = string | null | undefined;

/**
 * One cached area a change refreshes: every query under `key`, or under
 * `[feature, ...parts]` in every scope.
 */
export type Sweep =
	| { readonly key: QueryKey }
	| { readonly everyScope: readonly [feature: string, ...parts: string[]] };

/** A named fact about what changed, and the cached areas it refreshes. */
export interface Effect {
	readonly name: keyof typeof EFFECTS;
	readonly sweeps: readonly Sweep[];
}

const LIBRARY_MANAGEMENT = 'library-management';

/**
 * What can change, and what each change refreshes. Mutations and server
 * events name a fact here instead of listing keys themselves.
 */
export const EFFECTS = {
	/** Catalog rows moved: a scan, an import, a retag, a merge or a removal. */
	catalogChanged: (userId: UserId): Sweep[] => [
		{ everyScope: [LibraryQueryKeyFactory.feature] },
		{ everyScope: [LIBRARY_MANAGEMENT] },
		{ everyScope: [ArtistQueryKeyFactory.feature] },
		{ everyScope: [HomeQueryKeyFactory.feature] },
		{ everyScope: [DiscoverQueryKeyFactory.feature] },
		{ everyScope: [GenreQueryKeyFactory.feature] },
		{ everyScope: [LyricsQueryKeyFactory.feature] },
		{ everyScope: [LOCAL_KEYS.feature] },
		{ everyScope: [RatingQueryKeyFactory.feature] },
		{ everyScope: [DownloadQueryKeyFactory.feature] },
		{ everyScope: [DropImportQueryKeyFactory.feature] },
		{ key: SearchQueryKeyFactory.v3.root(userId) }
	],
	/** Scans, identification or operations moved without touching the catalog. */
	libraryActivityChanged: (userId: UserId): Sweep[] => [
		{ key: LibraryQueryKeyFactory.activity(userId) }
	],
	/** The admin's view of library runs, reviews and managed changes. */
	libraryOperationsChanged: (): Sweep[] => [
		{ everyScope: [LibraryQueryKeyFactory.feature, 'operations'] },
		{ key: LibraryQueryKeyFactory.reviewsPrefix() },
		{ everyScope: [LIBRARY_MANAGEMENT] }
	],
	/** The local library lists. */
	localChanged: (): Sweep[] => [{ everyScope: [LOCAL_KEYS.feature] }],
	/** The download queue moved: a task, its status, its source or a held file. */
	downloadsChanged: (userId: UserId): Sweep[] => [
		{ key: DownloadQueryKeyFactory.tasksPrefix(userId) }
	],
	/** The caller's requests. */
	requestsChanged: (userId: UserId): Sweep[] => [{ key: RequestQueryKeyFactory.all(userId) }],
	/** One manual album search moved. */
	searchJobChanged: (userId: UserId, jobId: string): Sweep[] => [
		{ key: DownloadQueryKeyFactory.searchJob(userId, jobId) }
	],
	/** The caller's manual album searches, whichever moved. */
	searchJobsChanged: (userId: UserId): Sweep[] => [
		{ key: key(userScope(userId), DownloadQueryKeyFactory.feature, 'search') }
	],
	/** The caller's drop imports, and the held files they leave. */
	dropImportsChanged: (userId: UserId): Sweep[] => [
		{ everyScope: [DropImportQueryKeyFactory.feature] },
		{ key: DownloadQueryKeyFactory.heldPrefix(userId) }
	],
	/** The caller's playlists, the weekly mix included. */
	playlistsChanged: (userId: UserId): Sweep[] => [{ key: PlaylistQueryKeyFactory.root(userId) }],
	/** New releases from followed artists. */
	newReleasesChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.newReleasesUnseen(userId) }
	],
	/** Concerts for followed artists. */
	concertsChanged: (userId: UserId): Sweep[] => [
		{ key: FollowQueryKeyFactory.concertsUnseen(userId) },
		{ key: FollowQueryKeyFactory.concerts(userId) }
	],
	/** The caller's wanted albums. */
	wantedChanged: (userId: UserId): Sweep[] => [{ key: WantedQueryKeyFactory.list(userId) }],
	/** The caller asked for fewer recommendations like one album. */
	recommendationsChanged: (userId: UserId): Sweep[] => [
		{ key: DiscoverQueryKeyFactory.v3.ignored(userId) },
		{ key: DiscoverQueryKeyFactory.v3.home(userId) }
	]
};

export const catalogChanged = (userId: UserId): Effect => ({
	name: 'catalogChanged',
	sweeps: EFFECTS.catalogChanged(userId)
});
export const libraryActivityChanged = (userId: UserId): Effect => ({
	name: 'libraryActivityChanged',
	sweeps: EFFECTS.libraryActivityChanged(userId)
});
export const libraryOperationsChanged = (): Effect => ({
	name: 'libraryOperationsChanged',
	sweeps: EFFECTS.libraryOperationsChanged()
});
export const localChanged = (): Effect => ({
	name: 'localChanged',
	sweeps: EFFECTS.localChanged()
});
export const downloadsChanged = (userId: UserId): Effect => ({
	name: 'downloadsChanged',
	sweeps: EFFECTS.downloadsChanged(userId)
});
export const requestsChanged = (userId: UserId): Effect => ({
	name: 'requestsChanged',
	sweeps: EFFECTS.requestsChanged(userId)
});
export const searchJobChanged = (userId: UserId, jobId: string): Effect => ({
	name: 'searchJobChanged',
	sweeps: EFFECTS.searchJobChanged(userId, jobId)
});
export const searchJobsChanged = (userId: UserId): Effect => ({
	name: 'searchJobsChanged',
	sweeps: EFFECTS.searchJobsChanged(userId)
});
export const dropImportsChanged = (userId: UserId): Effect => ({
	name: 'dropImportsChanged',
	sweeps: EFFECTS.dropImportsChanged(userId)
});
export const playlistsChanged = (userId: UserId): Effect => ({
	name: 'playlistsChanged',
	sweeps: EFFECTS.playlistsChanged(userId)
});
export const newReleasesChanged = (userId: UserId): Effect => ({
	name: 'newReleasesChanged',
	sweeps: EFFECTS.newReleasesChanged(userId)
});
export const concertsChanged = (userId: UserId): Effect => ({
	name: 'concertsChanged',
	sweeps: EFFECTS.concertsChanged(userId)
});
export const wantedChanged = (userId: UserId): Effect => ({
	name: 'wantedChanged',
	sweeps: EFFECTS.wantedChanged(userId)
});
export const recommendationsChanged = (userId: UserId): Effect => ({
	name: 'recommendationsChanged',
	sweeps: EFFECTS.recommendationsChanged(userId)
});

function filterFor(sweep: Sweep) {
	return 'key' in sweep ? { queryKey: sweep.key } : everyScope(...sweep.everyScope);
}

/** Refreshes every area the given facts sweep, each area once. */
export async function applyEffects(...effects: Effect[]): Promise<void> {
	const sweeps = new Map<string, Sweep>();
	for (const { sweeps: list } of effects) {
		for (const sweep of list) sweeps.set(JSON.stringify(sweep), sweep);
	}
	await Promise.all(
		[...sweeps.values()].map((sweep) => invalidateQueriesWithPersister(filterFor(sweep)))
	);
}
