import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

const apiPost = vi.hoisted(() => vi.fn());

vi.mock('$lib/api/client', () => ({
	api: { v3: { POST: apiPost } }
}));

const invalidate = vi.hoisted(() => vi.fn().mockResolvedValue(undefined));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: invalidate
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { actOnLibraryReview, type ReviewAction } from './LibraryReviewMutations.svelte';

// Review-local layer keys - swept for every action.
const REVIEW_LOCAL_KEYS = [
	['library', 'reviews'],
	['library', 'activity'],
	['library', 'operations']
];
// Catalog layer keys - only for catalog-mutating actions.
const CATALOG_KEYS = [['library'], ['artist'], ['home'], ['discover']];

beforeEach(() => {
	vi.clearAllMocks();
	apiPost.mockResolvedValue({});
});

describe('review action invalidation scoping', () => {
	it.each<ReviewAction>(['detach', 'exclude', 'restore'])(
		'sweeps the catalog layer for catalog-mutating action %s',
		async (action) => {
			const mutation = actOnLibraryReview() as unknown as {
				onSuccess: (
					result: unknown,
					input: { reviewId: string; action: ReviewAction }
				) => Promise<void>;
			};

			await mutation.onSuccess({}, { reviewId: 'review-1', action });

			const keys = invalidate.mock.calls.map(([filters]) => filters.queryKey);
			for (const key of [...REVIEW_LOCAL_KEYS, ...CATALOG_KEYS]) {
				expect(keys).toContainEqual(key);
			}
		}
	);

	it.each<ReviewAction>(['dismiss', 'keep_tagged'])(
		'keeps %s review-local: no library/artist/home/discover sweep',
		async (action) => {
			const mutation = actOnLibraryReview() as unknown as {
				onSuccess: (
					result: unknown,
					input: { reviewId: string; action: ReviewAction }
				) => Promise<void>;
			};

			await mutation.onSuccess({}, { reviewId: 'review-1', action });

			const keys = invalidate.mock.calls.map(([filters]) => filters.queryKey);
			for (const key of REVIEW_LOCAL_KEYS) {
				expect(keys).toContainEqual(key);
			}
			for (const key of CATALOG_KEYS) {
				expect(keys).not.toContainEqual(key);
			}
		}
	);
});
