import { page } from '@vitest/browser/context';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import '../../../app.css';
import LibraryReviewTable from './LibraryReviewTable.svelte';

const item = {
	id: 'review-1',
	state: 'needs_review' as const,
	reason_code: 'AMBIGUOUS',
	local_album_id: 'album-1',
	local_track_id: null,
	album_title: 'Local Album',
	album_artist_name: 'Local Artist',
	year: 2024,
	track_count: 9,
	metadata_incomplete_count: 0,
	root_id: 'root-1',
	relative_path: 'Artist/Album',
	effective_policy: 'automatic',
	exclusion_source: null,
	release_group_mbid: null,
	identity_source: null,
	candidate_count: 2,
	evidence_summary: {},
	active_job_state: null,
	created_at: 1,
	updated_at: 2,
	row_revision: 1
};

async function renderTable(items = [item], filtered = false, state?: string) {
	return await render(LibraryReviewTable, {
		props: {
			items,
			selectedIds: [],
			filtered,
			state,
			onselectionchange: vi.fn(),
			onreview: vi.fn()
		}
	} as unknown as Parameters<typeof render>[1]);
}

afterEach(async () => {
	await page.viewport(1280, 720);
});

describe('LibraryReviewTable responsive presentation', () => {
	it('uses a readable table on desktop with explicit selection', async () => {
		await page.viewport(1280, 720);
		await renderTable();
		await expect.element(page.getByRole('table')).toBeVisible();
		await expect.element(page.getByRole('checkbox', { name: 'Select Local Album' })).toBeVisible();
		await expect.element(page.getByText('Several equally likely releases').first()).toBeVisible();
	});

	it('uses the signed empty state for albums kept with local metadata', async () => {
		await renderTable([], false, 'keep_tagged');
		await expect
			.element(page.getByText('No albums have been kept with local metadata yet.'))
			.toBeVisible();
	});

	it('switches to an equivalent review card on mobile', async () => {
		await page.viewport(390, 760);
		await renderTable();
		await expect.element(page.getByRole('heading', { name: 'Local Album' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Review' })).toBeVisible();
		await expect.element(page.getByRole('checkbox', { name: 'Select' })).toBeVisible();
	});

	it('uses distinct active and filtered empty-state copy', async () => {
		await renderTable([], false);
		await expect.element(page.getByText('No albums need identification review.')).toBeVisible();
		await renderTable([], true);
		await expect
			.element(page.getByText('No review items match these filters.').last())
			.toBeVisible();
	});
});
