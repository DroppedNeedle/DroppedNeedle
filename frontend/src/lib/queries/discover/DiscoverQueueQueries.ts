// The Discover queue's reads and writes. The deck and the sampler live
// outside components, so they reach the app's QueryClient through these.
import { createQuery, QueryObserver, queryOptions } from '@tanstack/svelte-query';

import { api, ApiError } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { queryClient, setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { STALE_LIVE, STALE_SLOW } from '$lib/queries/staleTime';
import { authStore } from '$lib/stores/authStore.svelte';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';
import { DiscoverV3Api } from './DiscoverV3Api';
import type { DeckCard } from './types';

export type QueueStatus = components['schemas']['DiscoverQueueStatusResponse'];

/** How often a building queue's status is checked, in milliseconds. */
const BUILD_POLL_MS = 4000;

export const getDiscoverQueueStatusQueryOptions = (userId: string | null | undefined) =>
	queryOptions({
		queryKey: DiscoverQueryKeyFactory.v3.queueStatus(userId),
		queryFn: ({ signal }): Promise<QueueStatus> =>
			api.global.v3.GET(DiscoverV3Api.queueStatus(), { signal }),
		enabled: Boolean(userId),
		staleTime: STALE_LIVE,
		// Never restored from the browser's copy: an old status could offer a
		// queue that is already taken.
		persister: undefined,
		refetchInterval: (query: { state: { data?: QueueStatus } }) =>
			query.state.data?.status === 'building' ? BUILD_POLL_MS : false
	});

export const getDiscoverQueueStatusQuery = () =>
	createQuery(() => getDiscoverQueueStatusQueryOptions(authStore.user?.id));

/** The current status, asked fresh; null when it cannot be read. */
export async function fetchDiscoverQueueStatus(): Promise<QueueStatus | null> {
	try {
		return await queryClient.fetchQuery(getDiscoverQueueStatusQueryOptions(authStore.user?.id));
	} catch {
		return null;
	}
}

/**
 * Hears each new status after this call, polling while a build runs. It does
 * not fetch on subscribe, so a status read before a build starts never
 * overwrites the build's own answer.
 */
export function watchDiscoverQueueStatus(listener: (status: QueueStatus) => void): () => void {
	const observer = new QueryObserver(queryClient, {
		...getDiscoverQueueStatusQueryOptions(authStore.user?.id),
		refetchOnMount: false,
		refetchOnWindowFocus: false
	});
	let last = observer.getCurrentResult().data;
	return observer.subscribe((result) => {
		if (!result.data || result.data === last) return;
		last = result.data;
		listener(result.data);
	});
}

/** Starts a build. A refusal comes back as an `error` status and is not cached. */
export async function generateDiscoverQueue(force: boolean): Promise<QueueStatus> {
	const options = getDiscoverQueueStatusQueryOptions(authStore.user?.id);
	try {
		const answer = await api.global.v3.POST(DiscoverV3Api.queueGenerate(), { force });
		const status: QueueStatus = {
			status: answer.status,
			queue_id: answer.queue_id ?? null,
			item_count: answer.item_count ?? null,
			error: answer.error ?? null
		};
		await queryClient.cancelQueries({ queryKey: options.queryKey, exact: true });
		await setQueryDataWithPersister(options.queryKey, status);
		return status;
	} catch (error) {
		return {
			status: 'error',
			error:
				error instanceof ApiError
					? `Server responded with ${error.status}`
					: 'Failed to trigger generation'
		};
	}
}

/** The built queue was taken, so the status no longer offers it. */
export function markDiscoverQueueConsumed(): Promise<void> {
	return setQueryDataWithPersister(
		DiscoverQueryKeyFactory.v3.queueStatus(authStore.user?.id),
		(old: QueueStatus | undefined) =>
			old ? { ...old, status: 'idle' as const, queue_id: null, item_count: null } : old
	);
}

/** Takes the built queue. */
export function fetchDiscoverQueue() {
	return queryClient.fetchQuery({
		queryKey: DiscoverQueryKeyFactory.v3.queue(authStore.user?.id, null),
		queryFn: ({ signal }) => api.global.v3.GET(DiscoverV3Api.queue(null), { signal }),
		staleTime: STALE_LIVE,
		persister: undefined
	});
}

/** Which of these release groups the library already holds. */
export function fetchQueueInLibrary(releaseGroupMbids: string[]) {
	return queryClient.fetchQuery({
		queryKey: DiscoverQueryKeyFactory.v3.queueValidate(authStore.user?.id, releaseGroupMbids),
		queryFn: ({ signal }) =>
			api.global.v3.POST(
				DiscoverV3Api.queueValidate(),
				{ release_group_mbids: releaseGroupMbids },
				{ signal }
			),
		staleTime: STALE_LIVE,
		persister: undefined
	});
}

/** The details a card shows beyond the queue's light rows. */
export function fetchQueueEnrichment(releaseGroupMbid: string) {
	return queryClient.fetchQuery({
		queryKey: DiscoverQueryKeyFactory.v3.queueEnrich(authStore.user?.id, releaseGroupMbid),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.queueEnrich(releaseGroupMbid), { signal }),
		staleTime: STALE_SLOW
	});
}

export interface QueueIgnoreItem {
	releaseGroupMbid: string;
	artistMbid: string;
	releaseName: string;
	artistName: string;
}

/** Asks for fewer recommendations like this one. */
export function ignoreQueueItem(item: QueueIgnoreItem) {
	return api.global.v3.POST(DiscoverV3Api.queueIgnore(), {
		release_group_mbid: item.releaseGroupMbid,
		artist_mbid: item.artistMbid,
		release_name: item.releaseName,
		artist_name: item.artistName
	});
}

/** A preview clip for one track. */
export function fetchTrackPreview(artist: string, track: string) {
	return queryClient.fetchQuery({
		queryKey: DiscoverQueryKeyFactory.v3.trackPreview(authStore.user?.id, artist, track),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.trackPreview(artist, track), { signal }),
		staleTime: STALE_SLOW
	});
}

/** Preview clips for an album's tracks. */
export function fetchAlbumPreview(artist: string, album: string) {
	return queryClient.fetchQuery({
		queryKey: DiscoverQueryKeyFactory.v3.albumPreview(authStore.user?.id, artist, album, null),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DiscoverV3Api.albumPreview(artist, album, null), { signal }),
		staleTime: STALE_SLOW
	});
}

/** Where the person is in a deck, kept so a reload resumes the same cards. */
export interface SavedDeck {
	items: DeckCard[];
	currentIndex: number;
	queueId: string;
	savedAt: number;
}

// A deck left for a day is not resumed; the next visit takes a fresh queue.
const SAVED_DECK_MAX_AGE_MS = 24 * 60 * 60 * 1000;

const savedDeckKey = () => DiscoverQueryKeyFactory.v3.savedDeck(authStore.user?.id);

/** The saved deck from memory or the browser's copy, or null. */
export async function readSavedDeck(): Promise<SavedDeck | null> {
	try {
		const key = savedDeckKey();
		const saved = await queryClient.fetchQuery({
			queryKey: key,
			// Nothing on the server holds it: a refetch after a broad sweep keeps
			// what is in memory, and a fresh start finds the browser's copy first.
			queryFn: (): SavedDeck | null => queryClient.getQueryData<SavedDeck | null>(key) ?? null,
			// Only the deck itself changes this entry, so it never goes stale.
			staleTime: Infinity,
			gcTime: Infinity
		});
		if (!saved || Date.now() - saved.savedAt > SAVED_DECK_MAX_AGE_MS) return null;
		return saved;
	} catch {
		return null;
	}
}

/** The saved deck in memory, for changes made while the deck is not open. */
export function peekSavedDeck(): SavedDeck | null {
	return queryClient.getQueryData<SavedDeck | null>(savedDeckKey()) ?? null;
}

export function saveDeck(deck: Omit<SavedDeck, 'savedAt'>): Promise<void> {
	const saved: SavedDeck | null = { ...deck, savedAt: Date.now() };
	return setQueryDataWithPersister(savedDeckKey(), saved);
}

export function forgetSavedDeck(): Promise<void> {
	const none: SavedDeck | null = null;
	return setQueryDataWithPersister(savedDeckKey(), none);
}
