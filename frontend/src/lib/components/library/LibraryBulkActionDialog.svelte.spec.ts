import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const operation = {
	id: 'job-1',
	kind: 'bulk_review_apply',
	state: 'succeeded',
	expected_work_count: 2,
	completed_count: 2,
	succeeded_count: 1,
	failed_count: 0,
	skipped_count: 1,
	control_request: 'none',
	terminal_code: null,
	row_revision: 3,
	event_revision: 2,
	created_at: 1,
	updated_at: 2,
	results: [],
	results_truncated: false,
	repair_summary: null
};

const h = vi.hoisted(() => ({
	preview: vi.fn(),
	apply: vi.fn(),
	previewData: {
		preview_token: 'preview-1',
		action: 'retry',
		eligible_count: 2,
		ineligible_count: 0,
		stale_count: 0,
		reasons: {},
		album_count: 2,
		track_count: 20,
		root_count: 1,
		crosses_policy_boundaries: false,
		estimated_job_count: 2,
		playlist_reference_count: 0,
		history_reference_count: 0,
		requires_local_metadata_confirmation: false,
		common_candidate_keys: []
	} as Record<string, unknown>,
	jobs: {} as Record<string, unknown>,
	pause: vi.fn(),
	resume: vi.fn(),
	stop: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: { user: { id: 'admin-1' } } }));
vi.mock('$lib/queries/library/LibraryReviewMutations.svelte', () => ({
	previewBulkLibraryReview: () => ({
		mutateAsync: h.preview,
		get data() {
			return h.previewData;
		},
		isPending: false,
		isError: false
	}),
	applyBulkLibraryReview: () => ({ mutateAsync: h.apply, isPending: false })
}));
vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryOperationQuery: (getId: () => string | null) => ({
		get data() {
			const id = getId();
			return id ? h.jobs[id] : undefined;
		}
	})
}));
vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: (action: string) => ({
		mutateAsync: action === 'pause' ? h.pause : action === 'resume' ? h.resume : h.stop
	})
}));

import LibraryBulkActionDialog from './LibraryBulkActionDialog.svelte';

const selected = [
	{
		id: 'review-1',
		state: 'needs_review',
		reason_code: 'NO_CANDIDATE',
		local_album_id: 'album-1',
		local_track_id: null,
		album_title: 'One',
		album_artist_name: 'Artist',
		year: null,
		track_count: 10,
		metadata_incomplete_count: 0,
		root_id: 'root-1',
		relative_path: 'one',
		effective_policy: 'automatic',
		exclusion_source: null,
		release_group_mbid: null,
		identity_source: null,
		candidate_count: 0,
		evidence_summary: {},
		active_job_state: null,
		created_at: 1,
		updated_at: 1,
		row_revision: 2
	},
	{
		id: 'review-2',
		state: 'needs_review',
		reason_code: 'AMBIGUOUS',
		local_album_id: 'album-2',
		local_track_id: null,
		album_title: 'Two',
		album_artist_name: 'Artist',
		year: null,
		track_count: 10,
		metadata_incomplete_count: 0,
		root_id: 'root-1',
		relative_path: 'two',
		effective_policy: 'automatic',
		exclusion_source: null,
		release_group_mbid: null,
		identity_source: null,
		candidate_count: 2,
		evidence_summary: {},
		active_job_state: null,
		created_at: 1,
		updated_at: 1,
		row_revision: 3
	}
];

async function renderDialog(onclear = vi.fn(), allMatching = false) {
	return await render(LibraryBulkActionDialog, {
		props: {
			selected: allMatching ? [] : selected,
			allMatching,
			matchingCount: 120,
			filters: { state: 'needs_review' },
			catalogRevision: 8,
			onclear
		}
	} as unknown as Parameters<typeof render>[1]);
}

beforeEach(() => {
	vi.clearAllMocks();
	sessionStorage.clear();
	h.jobs = {};
	h.preview.mockResolvedValue(h.previewData);
	h.apply.mockResolvedValue(operation);
});

describe('LibraryBulkActionDialog', () => {
	it('uses the server preview and starts one durable job', async () => {
		const clear = vi.fn();
		await renderDialog(clear);
		const opener = page.getByRole('button', { name: 'Retry...' });
		await opener.click();
		expect(h.preview).toHaveBeenCalledWith(
			expect.objectContaining({
				action: 'retry',
				selection: expect.objectContaining({
					review_ids: ['review-1', 'review-2'],
					expected_revisions: { 'review-1': 2, 'review-2': 3 },
					catalog_revision: 8
				})
			})
		);
		await expect.element(page.getByText('2 eligible')).toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Preview bulk retry' })).toHaveFocus();
		await page.getByRole('button', { name: 'Close', exact: true }).click();
		await expect.element(opener).toHaveFocus();
		await opener.click();
		await page.getByRole('button', { name: 'Apply to 2' }).click();
		expect(h.apply).toHaveBeenCalledTimes(1);
		expect(sessionStorage.getItem('droppedneedle:library-bulk-job:admin-1')).toBe('job-1');
		expect(clear).toHaveBeenCalled();
	});

	it('recovers the stored terminal result after refresh without replaying Apply', async () => {
		sessionStorage.setItem('droppedneedle:library-bulk-job:admin-1', 'job-1');
		h.jobs = { 'job-1': operation };
		await renderDialog();
		await expect.element(page.getByText('Bulk review · succeeded')).toBeVisible();
		await expect.element(page.getByText('2 complete · 1 skipped · 0 failed')).toBeVisible();
		expect(sessionStorage.getItem('droppedneedle:library-bulk-job:admin-1')).toBeNull();
		await page.getByRole('button', { name: 'Dismiss' }).click();
		await expect.element(page.getByText('Bulk review · succeeded')).not.toBeInTheDocument();
		expect(h.apply).not.toHaveBeenCalled();
	});

	it('distinguishes the full filtered result from the current page', async () => {
		await renderDialog(vi.fn(), true);
		await expect.element(page.getByText('All 120 matching selected')).toBeVisible();
		await page.getByRole('button', { name: 'Retry...' }).click();
		expect(h.preview).toHaveBeenCalledWith(
			expect.objectContaining({
				selection: expect.objectContaining({
					review_ids: [],
					expected_revisions: {},
					normalized_filter: expect.objectContaining({ state: 'needs_review' })
				})
			})
		);
	});

	it('previews and applies one automatically safe candidate shared by the selection', async () => {
		h.previewData = {
			...h.previewData,
			action: 'accept_candidate',
			eligible_count: 2,
			common_candidate_keys: ['rg-shared:release-shared']
		};
		await renderDialog();

		await page.getByRole('button', { name: 'Accept shared candidate...' }).click();
		await page
			.getByLabelText('Candidate available to every selected item')
			.selectOptions('rg-shared:release-shared');
		expect(h.preview).toHaveBeenLastCalledWith(
			expect.objectContaining({
				action: 'accept_candidate',
				candidate_key: 'rg-shared:release-shared'
			})
		);
		await page.getByRole('button', { name: 'Apply to 2' }).click();
		expect(h.apply).toHaveBeenCalledWith(
			expect.objectContaining({
				action: 'accept_candidate',
				candidate_key: 'rg-shared:release-shared'
			})
		);
	});
});
