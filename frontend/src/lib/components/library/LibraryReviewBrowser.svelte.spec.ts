import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { setLibraryReviewUrl } from '$lib/test/libraryReviewPageState.svelte';
import type { LibraryReviewFilters } from '$lib/queries/library/LibraryReviewQueries.svelte';

const h = vi.hoisted(() => ({
	goto: vi.fn(),
	filters: (() => ({})) as () => LibraryReviewFilters,
	reviewPage: {
		items: [
			{
				id: 'review-1',
				state: 'needs_review',
				reason_code: 'CONTRADICTORY',
				local_album_id: 'album-1',
				local_track_id: null,
				album_title: 'URL State Album',
				album_artist_name: 'State Artist',
				year: 2026,
				track_count: 2,
				metadata_incomplete_count: 0,
				root_id: 'root-1',
				relative_path: 'state/album',
				effective_policy: 'automatic',
				exclusion_source: null,
				release_group_mbid: null,
				identity_source: null,
				candidate_count: 1,
				evidence_summary: {},
				active_job_state: null,
				created_at: 1,
				updated_at: 2,
				row_revision: 3
			}
		],
		next_cursor: 'cursor-2',
		has_more: true,
		filtered_total: 20,
		counts_by_state: { needs_review: 40, keep_tagged: 5 } as Record<string, number>,
		counts_by_reason: {},
		counts_by_reason_filtered: {},
		counts_by_state_filtered: { needs_review: 12, keep_tagged: 3 } as Record<string, number>,
		catalog_revision: 9
	}
}));

vi.mock('$app/state', async () => {
	const state = await import('$lib/test/libraryReviewPageState.svelte');
	return { page: state.libraryReviewPage };
});
vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/stores/authStore.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/stores/authStore.svelte')>()),
	authStore: { user: { id: 'admin-1' } }
}));
vi.mock('$lib/queries/library/LibraryReviewQueries.svelte', () => ({
	getLibraryReviewsQuery: (filters: () => LibraryReviewFilters) => {
		h.filters = filters;
		return {
			isLoading: false,
			isError: false,
			data: {
				pages: [h.reviewPage]
			}
		};
	},
	getLibraryReviewQuery: () => ({ data: undefined, isLoading: true, isError: false })
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getLibraryPolicyTreeQuery: () => ({
		data: {
			roots: [
				{
					id: 'root-1',
					kind: 'root',
					label: 'Main library',
					path: '/music',
					policy: 'automatic',
					inherited_from_id: 'root-1',
					available: true,
					indexed_file_count: 2,
					on_disk_file_count: 2,
					children: []
				}
			]
		}
	})
}));
vi.mock('$lib/queries/library/LibraryReviewMutations.svelte', () => ({
	actOnLibraryReview: () => ({ mutateAsync: vi.fn(), isPending: false }),
	acceptLibraryReviewCandidate: () => ({ mutateAsync: vi.fn(), isPending: false }),
	retryLibraryReview: () => ({ mutateAsync: vi.fn(), isPending: false }),
	previewBulkLibraryReview: () => ({ mutateAsync: vi.fn(), isPending: false }),
	applyBulkLibraryReview: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryOperationQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn() })
}));

import LibraryReviewBrowser from './LibraryReviewBrowser.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.reviewPage.counts_by_state = { needs_review: 40, keep_tagged: 5 };
	h.reviewPage.counts_by_state_filtered = { needs_review: 12, keep_tagged: 3 };
	setLibraryReviewUrl(
		'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album'
	);
});

describe('LibraryReviewBrowser URL state', () => {
	it('owns filters and cursors in the URL and preserves them when opening and closing review', async () => {
		await render(LibraryReviewBrowser);

		expect(h.filters()).toEqual({
			cursor: 'cursor-1',
			state: undefined,
			reasonCode: 'CONTRADICTORY',
			rootId: 'root-1',
			policy: undefined,
			search: undefined,
			sort: 'album'
		});
		await page.getByRole('button', { name: 'Review' }).first().click();
		expect(h.goto).toHaveBeenLastCalledWith(
			'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album&review=review-1',
			{ noScroll: true, keepFocus: true }
		);

		setLibraryReviewUrl(
			'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album&review=review-1'
		);
		await expect.element(page.getByRole('button', { name: 'Close review detail' })).toBeVisible();
		await page.getByRole('button', { name: 'Close review detail' }).click();
		expect(h.goto).toHaveBeenLastCalledWith(
			'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album',
			{ noScroll: true, keepFocus: true, replaceState: true }
		);

		setLibraryReviewUrl(
			'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album'
		);
		await expect
			.element(page.getByRole('button', { name: 'Close review detail' }))
			.not.toBeInTheDocument();
		setLibraryReviewUrl(
			'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album&review=review-1'
		);
		await expect.element(page.getByRole('button', { name: 'Close review detail' })).toBeVisible();
		setLibraryReviewUrl(
			'/library/review?state=all&cursor=cursor-1&reason=CONTRADICTORY&root=root-1&sort=album'
		);
		await expect
			.element(page.getByRole('button', { name: 'Close review detail' }))
			.not.toBeInTheDocument();
	});

	it('clears the cursor when filters change and retains filters across pagination', async () => {
		await render(LibraryReviewBrowser);

		await page.getByRole('button', { name: 'First page' }).click();
		expect(h.goto).toHaveBeenLastCalledWith(
			'/library/review?state=all&reason=CONTRADICTORY&root=root-1&sort=album',
			expect.objectContaining({ noScroll: true, keepFocus: true })
		);
		await page.getByRole('button', { name: 'Next page' }).click();
		expect(h.goto).toHaveBeenLastCalledWith(
			'/library/review?cursor=cursor-2&state=all&reason=CONTRADICTORY&root=root-1&sort=album',
			expect.objectContaining({ noScroll: true, keepFocus: true })
		);
		await page.getByRole('combobox', { name: 'Review state' }).selectOptions('keep_tagged');
		expect(h.goto).toHaveBeenLastCalledWith(
			'/library/review?state=keep_tagged&reason=CONTRADICTORY&root=root-1&sort=album',
			expect.objectContaining({ noScroll: true, keepFocus: true })
		);
	});
});

describe('LibraryReviewBrowser state depths (N-02/T30)', () => {
	it('renders scoped per-state depths and filters by state on click', async () => {
		await render(LibraryReviewBrowser);

		await expect.element(page.getByRole('button', { name: 'Needs review · 12' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Keep as tagged · 3' })).toBeVisible();

		await page.getByRole('button', { name: 'Needs review · 12' }).click();
		expect(h.goto).toHaveBeenLastCalledWith(
			'/library/review?state=needs_review&reason=CONTRADICTORY&root=root-1&sort=album',
			expect.objectContaining({ noScroll: true, keepFocus: true })
		);
	});

	it('falls back to all-time state totals when the scoped field is absent', async () => {
		delete (h.reviewPage as Record<string, unknown>).counts_by_state_filtered;
		await render(LibraryReviewBrowser);

		await expect.element(page.getByRole('button', { name: 'Needs review · 40' })).toBeVisible();
		await expect.element(page.getByText('All-time totals')).toBeVisible();
	});
});

describe('LibraryReviewBrowser confirm lane', () => {
	it('collapses the banner to a quiet line when few editions remain', async () => {
		// The scoped counts carry the page's own state filter, so they only
		// ever contain the active lane.
		h.reviewPage.counts_by_state_filtered = { edition_to_confirm: 3 };
		setLibraryReviewUrl('/library/review?state=edition_to_confirm');
		await render(LibraryReviewBrowser);

		await expect.element(page.getByText(/3 editions to confirm\./)).toBeVisible();
		await expect
			.element(page.getByText('Edition to confirm - release group pinned, pressing unproven.'))
			.not.toBeInTheDocument();
	});

	it('shows a clear-queue summary linking to resolved rows', async () => {
		// Cross-lane totals come from the global counts: the scoped field is
		// empty on a drained lane.
		h.reviewPage.counts_by_state_filtered = {};
		h.reviewPage.counts_by_state = { needs_review: 40, keep_tagged: 5, resolved: 40 };
		setLibraryReviewUrl('/library/review?state=edition_to_confirm&cursor=cursor-9');
		await render(LibraryReviewBrowser);

		await expect.element(page.getByText('Edition queue is clear.')).toBeVisible();
		const link = page.getByRole('button', { name: 'View 40 resolved reviews' });
		await expect.element(link).toBeVisible();
		await link.click();
		// The jump drops the stale lane cursor: the count is an all-time total.
		expect(h.goto).toHaveBeenLastCalledWith('/library/review?state=resolved', {
			noScroll: true,
			keepFocus: true
		});
	});

	it('hides the clear-queue claim when filters narrow the lane', async () => {
		h.reviewPage.counts_by_state_filtered = {};
		h.reviewPage.counts_by_state = { needs_review: 40, keep_tagged: 5, resolved: 40 };
		setLibraryReviewUrl('/library/review?state=edition_to_confirm&reason=EDITION_UNCERTAIN');
		await render(LibraryReviewBrowser);

		// The table owns the filtered empty state; the lane makes no claim.
		await expect.element(page.getByText('Edition queue is clear.')).not.toBeInTheDocument();
	});

	it('shows the banner above the threshold', async () => {
		h.reviewPage.counts_by_state_filtered = { edition_to_confirm: 26 };
		setLibraryReviewUrl('/library/review?state=edition_to_confirm');
		await render(LibraryReviewBrowser);

		await expect
			.element(page.getByText('Edition to confirm - release group pinned, pressing unproven.'))
			.toBeVisible();
	});

	it('shows the quiet line at the threshold', async () => {
		h.reviewPage.counts_by_state_filtered = { edition_to_confirm: 25 };
		setLibraryReviewUrl('/library/review?state=edition_to_confirm');
		await render(LibraryReviewBrowser);

		await expect.element(page.getByText(/25 editions to confirm\./)).toBeVisible();
		await expect
			.element(page.getByText('Edition to confirm - release group pinned, pressing unproven.'))
			.not.toBeInTheDocument();
	});
});
