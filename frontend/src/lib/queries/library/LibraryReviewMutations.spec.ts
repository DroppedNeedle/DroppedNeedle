import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { POST: vi.fn() } }
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import type { Effect } from '../effects';
import { actOnLibraryReview, type ReviewAction } from './LibraryReviewMutations.svelte';

function declared(action: ReviewAction): string[] {
	const mutation = actOnLibraryReview() as unknown as {
		effects: (result: unknown, input: { reviewId: string; action: ReviewAction }) => Effect[];
	};
	return mutation.effects({}, { reviewId: 'review-1', action }).map((effect) => effect.name);
}

// The review-local layer: declared for every action.
const REVIEW_LOCAL = [
	'reviewsChanged',
	'anyLibraryActivityChanged',
	'libraryOperationsChanged',
	'searchResultsChanged'
];

describe('review action effects', () => {
	it.each<ReviewAction>(['detach', 'exclude', 'restore'])(
		'declares the catalog changed for catalog-mutating action %s',
		(action) => {
			expect(declared(action)).toEqual([...REVIEW_LOCAL, 'catalogChanged']);
		}
	);

	it.each<ReviewAction>(['dismiss', 'keep_tagged'])(
		'keeps %s review-local: the catalog is untouched',
		(action) => {
			expect(declared(action)).toEqual(REVIEW_LOCAL);
		}
	);
});
