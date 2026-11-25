import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import {
	applyEffects,
	createAppMutation,
	discoveryBatchChanged,
	discoveryBatchesChanged,
	downloadsChanged,
	type Effect,
	libraryCountsChanged,
	pickHeard,
	recommendationsChanged,
	tuningsChanged
} from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { type FeedbackTarget, undoDiscoverFeedback } from './feedback';
import { toastStore } from '$lib/stores/toast';
import type {
	DiscoveryBatchDetailV3,
	DiscoveryBatchItemStatusV3
} from './DiscoverV3Queries.svelte';

export type DiscoveryBatchCreateV3 = components['schemas']['DiscoveryBatchCreate'];
export type DiscoveryBatchRemoveResultV3 = components['schemas']['DiscoveryBatchRemoveResult'];

/** Takes back one thing the caller told Discover, from the list of them. */
export const createUndoDiscoverFeedbackMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ target, mbid }: { target: FeedbackTarget; mbid: string }) =>
			undoDiscoverFeedback(target, mbid),
		effects: () => [recommendationsChanged(authStore.user?.id)],
		onError: () => toastStore.show({ message: "Couldn't undo that.", type: 'error' })
	}));

export type TuningKindV3 = components['schemas']['TuningKind'];

/** "Less from this": one of the caller's artists counts for less in their picks. */
export const createLessFromSeedMutation = () =>
	createAppMutation(() => ({
		mutationFn: (artistMbid: string) =>
			api.v3.POST(v3('/api/v3/discover/tunings'), {
				kind: 'less_from_seed',
				target: artistMbid
			}),
		effects: () => [tuningsChanged(authStore.user?.id)],
		onError: () => toastStore.show({ message: "Couldn't save that.", type: 'error' })
	}));

/** Takes back one way the caller tuned Discover, from the list of them. */
export const createUndoTuningMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ kind, target }: { kind: TuningKindV3; target: string }) =>
			api.v3.DELETE(v3('/api/v3/discover/tunings/{kind}/{target}', { path: { kind, target } })),
		effects: () => [tuningsChanged(authStore.user?.id)],
		onError: () => toastStore.show({ message: "Couldn't undo that.", type: 'error' })
	}));

export type PickEventV3 = components['schemas']['DiscoverPickEventRequest'];

/**
 * Tells the pick record what the person saw happen to a pick: shown, its
 * sample started, or its album opened from the card. A sample marks the
 * pick heard on the week; nothing on the page waits on it.
 */
export const createPickEventMutation = () =>
	createAppMutation(() => ({
		retry: false,
		effects: (_, { event }) => (event === 'sampled' ? [pickHeard(authStore.user?.id)] : []),
		mutationFn: (event: PickEventV3) => api.v3.POST(v3('/api/v3/discover/picks/events'), event)
	}));

// Sweep only what the API result says changed:
// - create: download tasks shift when at least one item was actually requested
//   (skips change nothing outside the batch itself);
// - remove: tasks shift on cancelled requests or recycled albums; library
//   counts/recency move only when albums were removed.
// Seam: batch outcomes also create native requests, whose v3 keys belong to
// the requests slice; that slice extends this sweep when its keys land.
function invalidateAfterBatchChangeV3(
	result: DiscoveryBatchDetailV3 | DiscoveryBatchRemoveResultV3,
	batchId: string | null
): Promise<void> {
	const userId = authStore.user?.id;
	const effects: Effect[] = [discoveryBatchesChanged(userId)];
	if (batchId) effects.push(discoveryBatchChanged(userId, batchId));
	const isCreate = 'id' in result;
	const requested = isCreate && (result.items ?? []).some((item) => item.outcome === 'requested');
	const cancelledRequests = isCreate ? 0 : (result.cancelled_requests ?? 0);
	const removedAlbums = isCreate ? 0 : (result.removed_albums ?? 0);
	if (requested || cancelledRequests > 0 || removedAlbums > 0) {
		// pending-request state lives under the tasks prefix
		effects.push(downloadsChanged(userId));
	}
	if (removedAlbums > 0) {
		// The client holds no per-item release-group ids at removal time (batch
		// list summaries omit items), so album-detail keys cannot be targeted
		// individually; stats + recently-added carry the visible shift and any
		// unopened album page self-heals via the global staleTime window.
		effects.push(libraryCountsChanged(userId));
	}
	return applyEffects(...effects);
}

export async function createDiscoveryBatchV3(
	body: DiscoveryBatchCreateV3
): Promise<DiscoveryBatchDetailV3 | null> {
	try {
		const created = await api.v3.POST(v3('/api/v3/discover/batches'), body);
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
		const result = await api.v3.DELETE(
			v3('/api/v3/discover/batches/{batch_id}', {
				path: { batch_id: batchId },
				query: { remove_albums: removeAlbums }
			})
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
			if (result.waiting) {
				const albums = `${result.waiting} album${result.waiting === 1 ? '' : 's'}`;
				toastStore.show({
					message: `${albums} stayed in the batch. ${result.waiting_reason ?? ''}`.trim(),
					type: 'warning'
				});
			}
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
