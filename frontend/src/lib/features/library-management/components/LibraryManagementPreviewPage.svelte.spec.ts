import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import {
	collisionItem,
	h,
	detail,
	resetPreviewMocks
} from './libraryManagementPreviewTestFixtures';

vi.mock(
	'$app/navigation',
	async () => (await import('./libraryManagementPreviewTestFixtures')).navigationMock
);
vi.mock(
	'$lib/stores/authStore.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).authMock
);
vi.mock(
	'$lib/features/library-management/queries/LibraryPolicyQueries.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).policyQueriesMock
);
vi.mock(
	'$lib/features/library/queries/LibraryQueries.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).libraryQueriesMock
);
vi.mock(
	'$lib/features/library-management/queries/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).queriesMock
);
vi.mock(
	'$lib/features/library-management/queries/LibraryManagementMutations.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).mutationsMock
);

import LibraryManagementPreviewPage from './LibraryManagementPreviewPage.svelte';

beforeEach(resetPreviewMocks);

describe('LibraryManagementPreviewPage', () => {
	it('discloses native tags selected for explicit scrub', async () => {
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								eligibility: 'eligible',
								reason_code: null,
								collisions: [],
								diff: {
									...collisionItem.diff,
									path_changed: false,
									scrubbed_raw_tags: [
										{
											key: 'droppedneedle_acceptance_marker',
											value_kind: 'text',
											values: ['scrub-me-2026-08-07'],
											value_count: 1,
											truncated: false,
											sha256: 'a'.repeat(64)
										}
									]
								}
							}
						],
						next_cursor: null
					}
				]
			}
		};

		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('button', { name: 'Inspect exact diff for Track' }).click();
		await expect.element(page.getByText('Unmanaged tags to remove')).toBeVisible();
		await expect.element(page.getByText('droppedneedle_acceptance_marker')).toBeVisible();
		await expect.element(page.getByText('scrub-me-2026-08-07')).toBeVisible();
		await expect.element(page.getByText('Remove', { exact: true })).toBeVisible();
	});

	it('says how many already organized albums it left out', async () => {
		const summary = detail().summary as Record<string, unknown>;
		h.preview = {
			data: detail({
				summary: { ...summary, organized_album_count: 12, organized_file_count: 140 }
			}),
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('12 albums')).toBeVisible();
		await expect.element(page.getByText('(140 files)')).toBeVisible();
		await expect.element(page.getByText(/already organized left out/)).toBeVisible();
	});

	it('makes stale and expired plans impossible to apply', async () => {
		h.preview = {
			data: detail({ stale: true, expired: true, ready_for_confirmation: false }),
			isLoading: false,
			isError: false
		};
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await expect.element(page.getByText('This preview cannot be applied.')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.toBeDisabled();
	});

	it('confirms discard, forgets the apply token, and returns to the control room', async () => {
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('button', { name: 'Discard preview...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Discard this preview?' }))
			.toHaveFocus();
		await page.getByRole('button', { name: 'Discard preview', exact: true }).click();

		expect(h.discard).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: { expected_operation_row_revision: 7 }
		});
		expect(
			sessionStorage.getItem('droppedneedle:library-management:preview-token:preview-1')
		).toBeNull();
		expect(h.goto).toHaveBeenCalledWith('/library/management?tab=organize');
	});

	it('renders a discarded audit plan without any write action', async () => {
		h.preview = {
			data: detail({
				state: 'cancelled',
				ready_for_confirmation: false,
				terminal_code: 'PREVIEW_DISCARDED'
			}),
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('Discarded', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('This preview is no longer awaiting confirmation.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('button', { name: /Discard preview/ }))
			.not.toBeInTheDocument();
	});

	it('keeps activation previews read-only while exposing every file-level result', async () => {
		h.preview = {
			data: detail({ proposed_settings_revision: 'settings-2' }),
			isLoading: false,
			isError: false
		};
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('Activation dry run')).toBeVisible();
		await expect.element(page.getByText(/This page is read-only/)).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Library settings' }))
			.toHaveAttribute('href', '/settings?tab=library');
	});

	it('starts on the files that change, with tabs for the rest', async () => {
		const base = detail();
		h.preview = {
			data: detail({
				summary: {
					...(base.summary as Record<string, unknown>),
					item_count: 5,
					eligible_count: 3,
					warning_count: 0,
					blocked_count: 1,
					stale_count: 1,
					no_change_count: 1
				}
			}),
			isLoading: false,
			isError: false
		};
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByRole('tab', { name: /^Changing\s2$/ }))
			.toHaveAttribute('aria-selected', 'true');
		await expect.element(page.getByRole('tab', { name: /^Not changing\s3$/ })).toBeVisible();
		await expect.element(page.getByRole('tab', { name: /^All\s5$/ })).toBeVisible();
		expect(h.itemParams?.().scope).toBe('changing');

		await page.getByRole('tab', { name: /^Not changing/ }).click();
		expect(h.itemParams?.().scope).toBe('not_changing');
		await page.getByRole('tab', { name: /^All/ }).click();
		expect(h.itemParams?.().scope).toBeUndefined();
	});

	it('shows every file when a filter picks files that never change', async () => {
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('combobox', { name: /^Outcome/ }).selectOptions('blocked');
		await expect
			.element(page.getByRole('tab', { name: /^All/ }))
			.toHaveAttribute('aria-selected', 'true');
		expect(h.itemParams?.()).toMatchObject({ scope: undefined, eligibility: 'blocked' });
	});

	it('says so when nothing changes, and points to every file', async () => {
		h.items = { ...h.items, data: { pages: [{ items: [], next_cursor: null }] } };
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect.element(page.getByText('Nothing in this preview changes a file.')).toBeVisible();
		await page.getByRole('button', { name: 'Show all files' }).click();
		await expect
			.element(page.getByRole('tab', { name: /^All/ }))
			.toHaveAttribute('aria-selected', 'true');
	});

	it('filters by an artist from the library search, and offers no unhonoured filters', async () => {
		h.searchArtists = [{ id: 'artist-1', name: 'Artist One' }];
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByText('Artist and release filters').click();
		await page.getByRole('textbox', { name: 'Find an artist or release' }).fill('Artist');
		await page.getByRole('button', { name: 'Artist · Artist One' }).click();
		expect(h.itemParams?.()).toMatchObject({ artistId: 'artist-1' });
		await expect.element(page.getByText('Artist: Artist One')).toBeVisible();
		await expect.element(page.getByText('Collision class')).not.toBeInTheDocument();
		await expect.element(page.getByText('Lossy representation')).not.toBeInTheDocument();
		await expect.element(page.getByText('Preserved / local override')).not.toBeInTheDocument();
	});
});
