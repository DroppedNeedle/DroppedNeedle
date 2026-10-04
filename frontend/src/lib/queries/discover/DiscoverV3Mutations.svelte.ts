import { createMutation } from '@tanstack/svelte-query';
import { untrack } from 'svelte';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import {
	musicBrainzSourceKey,
	setMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import type { MusicBrainzSourceMode } from '$lib/queries/musicbrainz/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { discoverQueueDeck } from '$lib/stores/discoverQueueDeck.svelte';
import { toastStore } from '$lib/stores/toast';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';
import { DiscoverV3Api } from './DiscoverV3Api';
import type {
	DiscoveryBatchDetailV3,
	DiscoveryBatchItemStatusV3
} from './DiscoverV3Queries.svelte';

export type QueueIgnoreRequestV3 = components['schemas']['QueueIgnoreRequest'];
export type QueueGenerateRequestV3 = components['schemas']['QueueGenerateRequest'];
export type DiscoverActivityRequestV3 = components['schemas']['DiscoverActivityRequest'];
export type DiscoveryBatchCreateV3 = components['schemas']['DiscoveryBatchCreate'];
export type DiscoveryBatchRemoveResultV3 = components['schemas']['DiscoveryBatchRemoveResult'];

export interface IgnoreDiscoveryV3Item {
	releaseGroupMbid: string;
	artistMbid: string;
	releaseName: string;
	artistName: string;
}

export const getIgnoreDiscoveryV3Mutation = () =>
	createMutation(() => ({
		mutationFn: (item: IgnoreDiscoveryV3Item) =>
			api.global.v3.POST(DiscoverV3Api.queueIgnore(), {
				release_group_mbid: item.releaseGroupMbid,
				artist_mbid: item.artistMbid,
				release_name: item.releaseName,
				artist_name: item.artistName
			}),
		onSuccess: async (_data, item) => {
			discoverQueueDeck.removeByMbid(item.releaseGroupMbid);
			toastStore.show({
				message: "We'll show fewer recommendations like this.",
				type: 'info'
			});
			await invalidateQueriesWithPersister({
				queryKey: DiscoverQueryKeyFactory.v3.home(authStore.user?.id)
			});
			await invalidateQueriesWithPersister({
				queryKey: DiscoverQueryKeyFactory.v3.ignored(authStore.user?.id)
			});
		},
		onError: () => toastStore.show({ message: "Couldn't save that preference.", type: 'error' })
	}));

export const getRefreshDiscoverV3Mutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(DiscoverV3Api.refresh()),
		onSuccess: () =>
			invalidateQueriesWithPersister({
				queryKey: DiscoverQueryKeyFactory.v3.home(authStore.user?.id)
			})
	}));

export const getGenerateDiscoverQueueV3Mutation = () =>
	createMutation(() => ({
		mutationFn: (body: QueueGenerateRequestV3) =>
			api.global.v3.POST(DiscoverV3Api.queueGenerate(), body),
		onSuccess: async () => {
			await invalidateQueriesWithPersister({
				queryKey: DiscoverQueryKeyFactory.v3.queueStatus(authStore.user?.id)
			});
			await invalidateQueriesWithPersister({
				queryKey: DiscoverQueryKeyFactory.v3.root(authStore.user?.id)
			});
		}
	}));

export const getQueuePreviewV3Mutation = () =>
	createMutation(() => ({
		retry: false,
		mutationFn: ({ mbid, signal }: { mbid: string; signal: AbortSignal }) =>
			api.global.v3.POST(DiscoverV3Api.queuePreview(mbid), undefined, {
				signal
			})
	}));

const KNOWN_SOURCE_MODES: MusicBrainzSourceMode[] = [
	'brainzmash',
	'official',
	'mirror',
	'community'
];

export async function recordDiscoverActivityV3(
	activity: DiscoverActivityRequestV3,
	signal?: AbortSignal
): Promise<void> {
	const userId = authStore.user?.id;
	const before = JSON.stringify(musicBrainzSourceKey());
	const source = await api.global.v3.POST(DiscoverV3Api.activity(), activity, { signal });
	const knownMode = KNOWN_SOURCE_MODES.find((mode) => mode === source.source_mode);
	if (
		!signal?.aborted &&
		userId === authStore.user?.id &&
		before === JSON.stringify(musicBrainzSourceKey()) &&
		knownMode !== undefined
	) {
		setMusicBrainzSourceScope(
			{ source_mode: knownMode, source_id: source.source_id, generation: source.generation },
			userId
		);
	}
}

export function useDiscoverActivityV3(
	getActivity: () => DiscoverActivityRequestV3 | null,
	getElement?: () => HTMLElement | undefined
): void {
	const mutation = createMutation(() => ({
		retry: false,
		mutationFn: (activity: DiscoverActivityRequestV3) => recordDiscoverActivityV3(activity)
	}));
	$effect(() => {
		const userId = authStore.user?.id;
		const activity = getActivity();
		const element = getElement?.();
		if (!userId || !activity || (getElement && !element)) return;
		let visible = !getElement;
		let entered = false;
		const signalEntry = () => {
			const active = visible && document.visibilityState === 'visible';
			if (active && !entered) untrack(() => mutation.mutate(activity));
			entered = active;
		};
		const onFocus = () => {
			entered = false;
			signalEntry();
		};
		const observer = element
			? new IntersectionObserver(([entry]) => {
					visible = entry.isIntersecting;
					signalEntry();
				})
			: null;
		if (element) observer?.observe(element);
		document.addEventListener('visibilitychange', signalEntry);
		window.addEventListener('focus', onFocus);
		signalEntry();
		return () => {
			observer?.disconnect();
			document.removeEventListener('visibilitychange', signalEntry);
			window.removeEventListener('focus', onFocus);
		};
	});
}

// Sweep only what the API result says changed:
// - create: download tasks shift when at least one item was actually requested
//   (skips change nothing outside the batch itself);
// - remove: tasks shift on cancelled requests or recycled albums; library
//   counts/recency move only when albums were removed.
// Seam: batch outcomes also create native requests, whose v3 keys belong to
// the requests slice; that slice extends this sweep when its keys land.
async function invalidateAfterBatchChangeV3(
	result: DiscoveryBatchDetailV3 | DiscoveryBatchRemoveResultV3,
	batchId: string | null
): Promise<void> {
	await invalidateQueriesWithPersister({
		queryKey: DiscoverQueryKeyFactory.v3.batches(authStore.user?.id)
	});
	if (batchId) {
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.v3.batch(authStore.user?.id, batchId)
		});
	}
	const isCreate = 'id' in result;
	const requested = isCreate && (result.items ?? []).some((item) => item.outcome === 'requested');
	const cancelledRequests = isCreate ? 0 : (result.cancelled_requests ?? 0);
	const removedAlbums = isCreate ? 0 : (result.removed_albums ?? 0);
	if (requested || cancelledRequests > 0 || removedAlbums > 0) {
		// pending-request state lives under the tasks prefix
		await invalidateQueriesWithPersister({
			queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
		});
	}
	if (removedAlbums > 0) {
		// The client holds no per-item release-group ids at removal time (batch
		// list summaries omit items), so album-detail keys cannot be targeted
		// individually; stats + recently-added carry the visible shift and any
		// unopened album page self-heals via the global staleTime window.
		const userId = authStore.user?.id;
		await invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.v3.stats(userId)
		});
		await invalidateQueriesWithPersister({
			queryKey: [...LibraryQueryKeyFactory.v3.root(userId), 'recently-added']
		});
	}
}

export async function createDiscoveryBatchV3(
	body: DiscoveryBatchCreateV3
): Promise<DiscoveryBatchDetailV3 | null> {
	try {
		const created = await api.global.v3.POST(DiscoverV3Api.batches(), body);
		const items: DiscoveryBatchItemStatusV3[] = created.items ?? [];
		const requested = items.filter((item) => item.outcome === 'requested').length;
		const skipped = items.length - requested;
		toastStore.show({
			message:
				`${requested} album${requested === 1 ? '' : 's'} requested` +
				(skipped ? ` · ${skipped} already yours or requested` : ''),
			type: 'success'
		});
		await invalidateAfterBatchChangeV3(created, created.id);
		return created;
	} catch (error) {
		toastStore.show({
			message: error instanceof Error ? error.message : "Couldn't create the batch",
			type: 'error'
		});
		return null;
	}
}

export async function removeDiscoveryBatchV3(
	batchId: string,
	removeAlbums: boolean
): Promise<DiscoveryBatchRemoveResultV3 | null> {
	try {
		const result = await api.global.v3.DELETE(
			DiscoverV3Api.batchRemove(batchId, removeAlbums)
		);
		const removed = result.removed_albums ?? 0;
		if (removeAlbums) {
			toastStore.show({
				message:
					`Removed ${removed} album${removed === 1 ? '' : 's'} to the recycle bin` +
					(result.cancelled_requests ? `, cancelled ${result.cancelled_requests} pending` : '') +
					(result.kept ? `, left ${result.kept} untouched` : ''),
				type: 'success'
			});
		} else {
			toastStore.show({ message: 'Batch record removed - albums kept', type: 'success' });
		}
		await invalidateAfterBatchChangeV3(result, batchId);
		return result;
	} catch (error) {
		toastStore.show({
			message: error instanceof Error ? error.message : "Couldn't remove the batch",
			type: 'error'
		});
		return null;
	}
}
