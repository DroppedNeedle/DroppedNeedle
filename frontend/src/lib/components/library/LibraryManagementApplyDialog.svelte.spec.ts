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

describe('LibraryManagementApplyDialog', () => {
	it('shows exact diffs and requires the private token plus typed apply confirmation', async () => {
		h.items = {
			...h.items,
			data: {
				pages: [
					{
						items: [
							{
								...collisionItem,
								artwork_choices: [
									{
										output_kind: 'external_art',
										image_type: 'front',
										blob_sha256: 'a'.repeat(64),
										source: 'cover_art_archive_release',
										format: 'jpeg',
										mime_type: 'image/jpeg',
										width: 1200,
										height: 1200,
										destination_relative_path: 'Artist/Album/cover.jpg'
									}
								]
							}
						],
						has_more: false,
						next_after_ordinal: null
					}
				]
			}
		};
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await expect.element(page.getByRole('heading', { name: 'Organization preview' })).toBeVisible();
		await expect.element(page.getByText('1 tag change', { exact: true })).toBeVisible();
		await expect.element(page.getByText('1 path change', { exact: true })).toBeVisible();
		await expect.element(page.getByText('0 artwork changes', { exact: true })).toBeVisible();
		await expect.element(page.getByText('/secret/music')).not.toBeInTheDocument();
		await page.getByText('Inspect exact diff').click();
		await expect.element(page.getByText('Old title')).toBeVisible();
		await expect.element(page.getByText('Track', { exact: true }).first()).toBeVisible();
		await expect.element(page.getByText('External Art · Front')).toBeVisible();
		await expect
			.element(page.getByRole('img', { name: 'Front preview' }))
			.toHaveAttribute(
				'src',
				`/api/v3/library/management/previews/preview-1/items/0/artwork/${'a'.repeat(64)}`
			);
		await expect
			.element(page.getByText(/Cover Art Archive Release · 1,200 × 1,200 px/))
			.toBeVisible();
		await expect.element(page.getByText('Artist/Album/cover.jpg')).toBeVisible();

		await page.getByRole('button', { name: /Write tags and organize 1 file/ }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Apply this exact preview?' }))
			.toHaveFocus();
		await expect.element(page.getByRole('button', { name: 'Apply exact preview' })).toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('APPLY LIBRARY MANAGEMENT');
		await expect.element(page.getByRole('button', { name: 'Apply exact preview' })).toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Apply exact preview' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: expect.objectContaining({
				preview_token: 'private-token',
				expected_operation_row_revision: 7,
				confirmation: true
			})
		});
		expect(h.goto).toHaveBeenCalledWith('/library/management/operations/preview-1');
	});

	it('uses the new token when navigation reuses the page for another preview', async () => {
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'source-token'
		);
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:resolution-1',
			'resolution-token'
		);
		const view = await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await view.rerender({ jobId: 'resolution-1' });
		await page.getByRole('button', { name: /Write tags and organize 1 file/ }).click();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Apply exact preview' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'resolution-1',
			request: expect.objectContaining({ preview_token: 'resolution-token' })
		});
	});

	it('resumes apply after a browser restart by reissuing the sealed token', async () => {
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await vi.waitFor(() => expect(h.reissue).toHaveBeenCalledWith({ jobId: 'preview-1' }));
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize 1 file/ }))
			.toBeEnabled();
		await expect
			.element(page.getByText('The private apply token is not in this browser session.'))
			.not.toBeInTheDocument();
		expect(sessionStorage.getItem('droppedneedle:library-management:preview-token:preview-1')).toBe(
			'resumed-token'
		);

		await page.getByRole('button', { name: /Write tags and organize 1 file/ }).click();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Apply exact preview' }).click();

		expect(h.apply).toHaveBeenCalledWith({
			jobId: 'preview-1',
			request: expect.objectContaining({
				preview_token: 'resumed-token',
				expected_operation_row_revision: 7,
				confirmation: true
			})
		});
	});

	it('keeps apply disabled with a clause-specific reason and a token retry when reissue is denied', async () => {
		h.reissue.mockRejectedValueOnce(new Error('Forbidden'));
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await expect
			.element(page.getByText('Applying is the first write action · no files changed'))
			.toBeVisible();
		await expect
			.element(page.getByText(/Apply is disabled: the private apply token is missing/))
			.toBeVisible();
		expect(h.reissue).toHaveBeenCalledWith({ jobId: 'preview-1' });
		await expect
			.element(page.getByRole('button', { name: /Write tags and organize/ }))
			.toBeDisabled();
		await page.getByRole('button', { name: 'Retry token resume' }).click();
		await vi.waitFor(() => expect(h.reissue).toHaveBeenCalledTimes(2));
	});

	it.each([
		{
			mode: 'undo',
			button: 'Undo this operation for 1 file',
			title: 'Undo this operation from this exact preview?',
			confirm: 'Undo operation',
			detail: /does not restore the broader original baseline/
		},
		{
			mode: 'baseline_restore',
			button: 'Restore original state for 1 file',
			title: 'Restore these original baselines?',
			confirm: 'Restore original state',
			detail: /broader than Undo and leaves those files unmanaged/
		},
		{
			mode: 'duplicate_resolution',
			button: 'Apply collision resolution for 1 file',
			title: 'Apply this exact collision resolution?',
			confirm: 'Apply collision resolution',
			detail: /No destination is overwritten and no duplicate is deleted automatically/
		}
	])('uses consequence-specific confirmation copy for $mode', async (example) => {
		h.preview = { data: detail({ mode: example.mode }), isLoading: false, isError: false };
		sessionStorage.setItem(
			'droppedneedle:library-management:preview-token:preview-1',
			'private-token'
		);
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });

		await page.getByRole('button', { name: example.button }).click();
		await expect.element(page.getByRole('heading', { name: example.title })).toHaveFocus();
		await expect.element(page.getByText(example.detail)).toBeVisible();
		await expect.element(page.getByRole('textbox', { name: 'Type CONFIRM' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: example.confirm, exact: true }))
			.toBeDisabled();
	});
});
