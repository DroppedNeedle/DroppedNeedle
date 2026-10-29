import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { toastStore } from '$lib/stores/toast';
import { authStore } from '$lib/stores/authStore.svelte';
import { LibraryV3Api, reviewFilterQuery } from './LibraryV3Api';
import type { LibraryReviewParams } from './LibraryQueryKeyFactory';
import {
	anyLibraryActivityChanged,
	applyEffects,
	catalogChanged,
	createAppMutation,
	type Effect,
	libraryOperationsChanged,
	reviewsChanged,
	searchResultsChanged
} from '$lib/queries/effects';

// Two layers: the review-local one (reviews, activity, operations, search)
// always; the catalog one only for actions that change what the library shows.
function reviewState(options?: { catalog?: boolean }): Effect[] {
	const userId = authStore.user?.id;
	return [
		reviewsChanged(),
		anyLibraryActivityChanged(),
		libraryOperationsChanged(),
		searchResultsChanged(userId),
		...(options?.catalog ? [catalogChanged(userId)] : [])
	];
}

// A refused single action usually means the review changed under the
// person: the list reloads so they decide on what it shows now.
const reloadReviews = () => applyEffects(reviewsChanged());

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
			return api.v3.POST(LibraryV3Api.keepTaggedReview(reviewId), body);
		case 'dismiss':
			return api.v3.POST(LibraryV3Api.dismissReview(reviewId), body);
		case 'detach':
			return api.v3.POST(LibraryV3Api.detachReview(reviewId), body);
		case 'exclude':
			return api.v3.POST(LibraryV3Api.excludeReview(reviewId), body);
		case 'restore':
			return api.v3.POST(LibraryV3Api.restoreReview(reviewId), body);
	}
}

/** Settle one review. `keep_tagged` and `dismiss` leave the catalog as it is. */
export function actOnLibraryReview() {
	return createAppMutation(() => ({
		mutationFn: async (input: ReviewActionInput) => {
			await post(input);
		},
		onSuccess: (_result, input) => {
			toastStore.show({ message: ACTION_TOAST[input.action], type: 'success' });
		},
		effects: (_result, input) =>
			reviewState({ catalog: input.action !== 'dismiss' && input.action !== 'keep_tagged' }),
		onError: reloadReviews
	}));
}

export function acceptLibraryReviewCandidate() {
	return createAppMutation(() => ({
		mutationFn: async (input: { reviewId: string; revision: number; candidateKey: string }) => {
			await api.v3.POST(LibraryV3Api.approveReview(input.reviewId), {
				candidate_key: input.candidateKey,
				expected_revision: input.revision
			});
		},
		effects: () => reviewState({ catalog: true }),
		onSuccess: () => {
			toastStore.show({ message: 'Release selected', type: 'success' });
		},
		onError: reloadReviews
	}));
}

export function retryLibraryReview() {
	return createAppMutation(() => ({
		mutationFn: (input: { reviewId: string; revision: number; confirmLocalMetadata?: boolean }) =>
			api.v3.POST(LibraryV3Api.retryReview(input.reviewId), {
				expected_revision: input.revision,
				confirm_local_metadata: input.confirmLocalMetadata ?? false
			}),
		effects: () => reviewState(),
		onSuccess: () => {
			toastStore.show({ message: 'Identification started again', type: 'success' });
		},
		onError: reloadReviews
	}));
}

/** Start identification again for every album with a pending review. */
export function retryPendingLibraryReviews() {
	return createAppMutation(() => ({
		mutationFn: (input: {
			confirmLocalMetadata: boolean;
			filter?: Omit<LibraryReviewParams, 'limit'>;
			rootIds?: string[];
			preview?: boolean;
		}) =>
			api.v3.POST(LibraryV3Api.retryPendingReviews(), {
				confirm_local_metadata: input.confirmLocalMetadata,
				filter: { state: 'pending', ...reviewFilterQuery(input.filter ?? {}) },
				root_ids: input.rootIds ?? [],
				preview: input.preview ?? false
			}),
		effects: (result) => (result.preview ? [] : reviewState())
	}));
}

export type BulkReviewAction = components['schemas']['BulkAction'];
export type BulkReviewPreview = components['schemas']['ReviewBulkPreviewResponse'];
export type BulkReviewResult = components['schemas']['ReviewBulkResponse'];

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
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (selection: BulkReviewSelection) =>
			api.v3.POST(LibraryV3Api.bulkReviewPreview(), bulkBody(selection))
	}));
}

export function applyBulkLibraryReviews() {
	return createAppMutation(() => ({
		mutationFn: (input: { selection: BulkReviewSelection; previewToken: string }) =>
			api.v3.POST(LibraryV3Api.bulkReviewApply(), bulkBody(input.selection, input.previewToken)),
		effects: (_result, input) => reviewState({ catalog: input.selection.action !== 'keep_tagged' })
	}));
}
