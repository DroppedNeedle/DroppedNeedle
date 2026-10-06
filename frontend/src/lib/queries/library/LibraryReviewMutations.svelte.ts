import { createMutation } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { searchStore } from '$lib/stores/search';
import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import type {
	BulkReviewAction,
	BulkReviewPreviewResponse,
	BulkReviewSelection,
	CandidateAcceptanceRequest,
	OperationResponse,
	ReviewActionRequest,
	ReviewActionResponse
} from './LibraryOperationsTypes';

// Two-layer sweep: the review-local layer (reviews list/detail, activity,
// operations) always runs; the catalog layer (library ALL + artist + home +
// discover) runs only for actions that change catalog rows.
async function invalidateReviewState(
	reviewId?: string,
	options?: { catalog?: boolean }
): Promise<void> {
	searchStore.clear();
	await Promise.all([
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.reviewsPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.activityPrefix() }),
		invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.operationsPrefix() }),
		...(reviewId
			? [invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.review(reviewId) })]
			: []),
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

export type ReviewAction = 'keep_tagged' | 'detach_keep_tagged' | 'exclude' | 'restore' | 'dismiss';

// Keeping the album as tagged is the v3 reject action and choosing a
// candidate is v3 approve; v3 carries no revision guards for either. The
// other review actions have no v3 route yet (see the waiting-on-backend list
// in eslint.config.js).
export function actOnLibraryReview(action: ReviewAction) {
	return createMutation(() => ({
		mutationFn: async (input: { reviewId: string; body: ReviewActionRequest }) => {
			if (action === 'keep_tagged') {
				await api.global.v3.POST(LibraryV3Api.rejectReview(input.reviewId));
				return;
			}
			const url =
				action === 'detach_keep_tagged'
					? API.library.reviewDetachKeepTagged(input.reviewId)
					: action === 'exclude'
						? API.library.reviewExclude(input.reviewId)
						: action === 'dismiss'
							? API.library.reviewDismiss(input.reviewId)
							: API.library.reviewRestore(input.reviewId);
			await api.global.post<ReviewActionResponse>(url, input.body);
		},
		onSuccess: async (_result, input) => {
			// dismiss and keep_tagged mutate no catalog rows: both run only the
			// common write set in NativeLibraryStore.apply_review_decision
			// (backend/infrastructure/persistence/native_library_store.py) - the
			// library_identification_reviews row, queued automatic-job
			// cancellation in library_identification_jobs, a
			// library_catalog_actions audit insert, and revision-counter bumps.
			// Those feed only review/activity/identification payloads; no home,
			// discover, library-list, or artist-page reader touches them. The
			// catalog tables move only under the other actions: local_tracks via
			// exclude/restore, local_(album|track)_external_identities via
			// detach_keep_tagged.
			const catalogMutating = action !== 'dismiss' && action !== 'keep_tagged';
			await invalidateReviewState(input.reviewId, { catalog: catalogMutating });
			toastStore.show({ message: 'Review decision saved', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not save the review decision', type: 'error' })
	}));
}

export function acceptLibraryReviewCandidate() {
	return createMutation(() => ({
		mutationFn: async (input: { reviewId: string; body: CandidateAcceptanceRequest }) => {
			await api.global.v3.POST(LibraryV3Api.approveReview(input.reviewId), {
				candidate_key: input.body.candidate_key
			});
		},
		onSuccess: async (_result, input) => {
			await invalidateReviewState(input.reviewId, { catalog: true });
			toastStore.show({ message: 'Release selected', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not select this release', type: 'error' })
	}));
}

export function retryLibraryReview() {
	return createMutation(() => ({
		mutationFn: (input: { reviewId: string; body: ReviewActionRequest }) =>
			api.global.post<OperationResponse>(API.library.reviewRetry(input.reviewId), input.body),
		onSuccess: async (_result, input) => {
			await invalidateReviewState(input.reviewId, { catalog: true });
			toastStore.show({ message: 'Identification retry started', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not retry identification', type: 'error' })
	}));
}

export function previewBulkLibraryReview() {
	return createMutation(() => ({
		mutationFn: (input: {
			action: BulkReviewAction;
			selection: BulkReviewSelection;
			candidate_key?: string | null;
		}) => api.global.post<BulkReviewPreviewResponse>(API.library.bulkReviewPreview(), input)
	}));
}

export function applyBulkLibraryReview() {
	return createMutation(() => ({
		mutationFn: (input: {
			preview_token: string;
			idempotency_key: string;
			action: BulkReviewAction;
			selection: BulkReviewSelection;
			candidate_key?: string | null;
			confirm_local_metadata?: boolean;
		}) => api.global.post<OperationResponse>(API.library.bulkReviewApply(), input),
		onSuccess: async () => {
			await invalidateReviewState(undefined, { catalog: true });
			toastStore.show({ message: 'Bulk review started', type: 'success' });
		},
		onError: () => toastStore.show({ message: 'Could not start the bulk review', type: 'error' })
	}));
}
