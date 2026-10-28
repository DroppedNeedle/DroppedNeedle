import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { searchStore } from '$lib/stores/search';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api, reviewFilterQuery } from './LibraryV3Api';
import type { LibraryReviewParams } from './LibraryQueryKeyFactory';

// Two-layer sweep: the review-local layer (reviews, activity, operations)
// always runs; the catalog layer (library, artist, home, discover) runs only
// for actions that change what the library shows.
async function invalidateReviewState(options?: { catalog?: boolean }): Promise<void> {
	searchStore.clear();
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.reviewsPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.operationsPrefix() }),
		...(options?.catalog
			? [
					invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.all }),
					invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }),
					invalidateQueriesWithPersister({ queryKey: DiscoverQueryKeyFactory.prefix })
				]
			: [])
	]);
}

// A refused single action usually means the review changed under the
// person: the list reloads so they decide on what it shows now.
const reloadReviews = () =>
	invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.reviewsPrefix() });

export type ReviewAction = 'keep_tagged' | 'dismiss' | 'detach' | 'exclude' | 'restore';

const ACTION_TOAST: Record<ReviewAction, string> = {
	keep_tagged: 'Kept as tagged',
	dismiss: 'Review closed',
	detach: 'Match removed; the album keeps its tags. Undo it from the album page.',
	exclude: 'Album taken out of the library',
	restore: 'Album is back in the library'
};

/** One review action, decided on the review's `row_revision` as it was read. */
export interface ReviewActionInput {
	reviewId: string;
	revision: number;
	action: ReviewAction;
}

function post({ reviewId, revision, action }: ReviewActionInput) {
	const body = { expected_revision: revision };
	switch (action) {
		case 'keep_tagged':
			return api.global.v3.POST(LibraryV3Api.keepTaggedReview(reviewId), body);
		case 'dismiss':
			return api.global.v3.POST(LibraryV3Api.dismissReview(reviewId), body);
		case 'detach':
			return api.global.v3.POST(LibraryV3Api.detachReview(reviewId), body);
		case 'exclude':
			return api.global.v3.POST(LibraryV3Api.excludeReview(reviewId), body);
		case 'restore':
			return api.global.v3.POST(LibraryV3Api.restoreReview(reviewId), body);
	}
}

/** Settle one review. `keep_tagged` and `dismiss` leave the catalog as it is. */
export function actOnLibraryReview() {
	return createMutation(() => ({
		mutationFn: async (input: ReviewActionInput) => {
			await post(input);
		},
		onSuccess: async (_result, input) => {
			const catalog = input.action !== 'dismiss' && input.action !== 'keep_tagged';
			await invalidateReviewState({ catalog });
			toastStore.show({ message: ACTION_TOAST[input.action], type: 'success' });
		},
		onError: reloadReviews
	}));
}

export function acceptLibraryReviewCandidate() {
	return createMutation(() => ({
		mutationFn: async (input: { reviewId: string; revision: number; candidateKey: string }) => {
			await api.global.v3.POST(LibraryV3Api.approveReview(input.reviewId), {
				candidate_key: input.candidateKey,
				expected_revision: input.revision
			});
		},
		onSuccess: async () => {
			await invalidateReviewState({ catalog: true });
			toastStore.show({ message: 'Release selected', type: 'success' });
		},
		onError: reloadReviews
	}));
}

export function retryLibraryReview() {
	return createMutation(() => ({
		mutationFn: (input: { reviewId: string; revision: number; confirmLocalMetadata?: boolean }) =>
			api.global.v3.POST(LibraryV3Api.retryReview(input.reviewId), {
				expected_revision: input.revision,
				confirm_local_metadata: input.confirmLocalMetadata ?? false
			}),
		onSuccess: async () => {
			await invalidateReviewState();
			toastStore.show({ message: 'Identification started again', type: 'success' });
		},
		onError: reloadReviews
	}));
}

/** Start identification again for every album with a pending review. */
export function retryPendingLibraryReviews() {
	return createMutation(() => ({
		mutationFn: (input: {
			confirmLocalMetadata: boolean;
			filter?: Omit<LibraryReviewParams, 'limit'>;
			rootIds?: string[];
			preview?: boolean;
		}) =>
			api.global.v3.POST(LibraryV3Api.retryPendingReviews(), {
				confirm_local_metadata: input.confirmLocalMetadata,
				filter: { state: 'pending', ...reviewFilterQuery(input.filter ?? {}) },
				root_ids: input.rootIds ?? [],
				preview: input.preview ?? false
			}),
		onSuccess: async (result) => {
			if (!result.preview) await invalidateReviewState();
		}
	}));
}

export type BulkReviewAction = components['schemas']['BulkAction'];

/** Which reviews a bulk change takes: picked by hand, or every match of a filter. */
export interface BulkReviewSelection {
	action: BulkReviewAction;
	reviewIds: string[];
	filter: Omit<LibraryReviewParams, 'limit'>;
	candidateKey?: string;
}

function bulkBody(selection: BulkReviewSelection, previewToken?: string) {
	return {
		action: selection.action,
		review_ids: selection.reviewIds,
		filter: { state: selection.filter.state ?? 'pending', ...reviewFilterQuery(selection.filter) },
		candidate_key: selection.candidateKey ?? null,
		preview_token: previewToken ?? null
	};
}

export function previewBulkLibraryReviews() {
	return createMutation(() => ({
		mutationFn: (selection: BulkReviewSelection) =>
			api.global.v3.POST(LibraryV3Api.bulkReviewPreview(), bulkBody(selection))
	}));
}

export function applyBulkLibraryReviews() {
	return createMutation(() => ({
		mutationFn: (input: { selection: BulkReviewSelection; previewToken: string }) =>
			api.global.v3.POST(
				LibraryV3Api.bulkReviewApply(),
				bulkBody(input.selection, input.previewToken)
			),
		onSuccess: async (_result, input) => {
			await invalidateReviewState({ catalog: input.selection.action !== 'keep_tagged' });
		}
	}));
}
