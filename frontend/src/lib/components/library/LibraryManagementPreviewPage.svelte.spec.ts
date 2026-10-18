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
	'$lib/queries/library/LibraryPolicyQueries.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).policyQueriesMock
);
vi.mock(
	'$lib/queries/library/LibraryQueries.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).libraryQueriesMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementEvents',
	async () => (await import('./libraryManagementPreviewTestFixtures')).eventsMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementPreviewTestFixtures')).queriesMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementMutations.svelte',
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
						has_more: false,
						next_after_ordinal: null
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
});
