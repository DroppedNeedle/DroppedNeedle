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
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { ArtistQueryKeyFactory } from '../artist/ArtistQueryKeyFactory';
import { DiscoverQueryKeyFactory } from '../discover/DiscoverQueryKeyFactory';
import { HomeQueryKeyFactory } from '../HomeQueryKeyFactory';

// Review-local layer: one key of each, swept for every action.
const REVIEW_LOCAL_KEYS = [
	LibraryQueryKeyFactory.review('review-1'),
	LibraryQueryKeyFactory.activity('u'),
	LibraryQueryKeyFactory.currentRuns()
];
// Catalog layer: one key of each, only for catalog-mutating actions.
const CATALOG_KEYS = [
	LibraryQueryKeyFactory.catalog.stats('u'),
	ArtistQueryKeyFactory.basic('artist-1'),
	HomeQueryKeyFactory.home('u'),
	DiscoverQueryKeyFactory.v3.home('u')
];

type Filter = {
	queryKey?: readonly unknown[];
	predicate?: (query: { queryKey: readonly unknown[] }) => boolean;
};

/** Whether any invalidation reached this key, by key prefix or by predicate. */
function swept(queryKey: readonly unknown[]): boolean {
	return invalidate.mock.calls.some(([filter]) => {
		const { queryKey: prefix, predicate } = filter as Filter;
		if (predicate) return predicate({ queryKey });
		return !!prefix && prefix.every((part, i) => part === queryKey[i]);
	});
}

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

			for (const key of [...REVIEW_LOCAL_KEYS, ...CATALOG_KEYS]) {
				expect(swept(key), JSON.stringify(key)).toBe(true);
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

			for (const key of REVIEW_LOCAL_KEYS) {
				expect(swept(key), JSON.stringify(key)).toBe(true);
			}
			for (const key of CATALOG_KEYS) {
				expect(swept(key), JSON.stringify(key)).toBe(false);
			}
		}
	);
});
