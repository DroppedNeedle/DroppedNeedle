import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { h, roots, resetLibraryManagementMocks } from './libraryManagementTestFixtures';

vi.mock(
	'$lib/queries/library-management/LibraryManagementPreviewTokens',
	async () => (await import('./libraryManagementTestFixtures')).previewTokensMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementQueries.svelte',
	async () => (await import('./libraryManagementTestFixtures')).queriesMock
);
vi.mock(
	'$lib/queries/library-management/LibraryManagementMutations.svelte',
	async () => (await import('./libraryManagementTestFixtures')).mutationsMock
);

import SettingsLibraryManagement from './SettingsLibraryManagement.svelte';

beforeEach(resetLibraryManagementMocks);

describe('LibraryManagementPurgeDialog', () => {
	it('keeps irreversible baseline purge in advanced retention with impact and typed confirmation', async () => {
		h.purgeData = {
			baseline_count: 14,
			referenced_blob_count: 9,
			referenced_blob_bytes: 4096,
			blocked_journal_count: 0,
			active_restore_count: 0,
			impact_token: 'impact-token'
		};
		h.purgeImpact.mockResolvedValue(h.purgeData);
		h.purge.mockResolvedValue({
			purged_baseline_count: 14,
			detached_reference_count: 9,
			cleaned_blob_count: 9,
			existing: false
		});

		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByText('Retention, recycle, and refresh').click();
		await page.getByRole('button', { name: 'Purge baselines...' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Purge original baselines?' }))
			.toHaveFocus();
		await expect.element(page.getByText(/permanently removes 14 baselines/)).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Purge baselines', exact: true }))
			.toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('PURGE BASELINES');
		await expect
			.element(page.getByRole('button', { name: 'Purge baselines', exact: true }))
			.toBeDisabled();
		await page.getByRole('textbox', { name: /CONFIRM/ }).fill('CONFIRM');
		await page.getByRole('button', { name: 'Purge baselines', exact: true }).click();

		expect(h.purge).toHaveBeenCalledWith(
			expect.objectContaining({
				impact_token: 'impact-token',
				typed_confirmation: 'CONFIRM'
			})
		);
	});
});
