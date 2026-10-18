import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import {
	h,
	multiDiscNamingScriptId,
	roots,
	resetLibraryManagementMocks
} from './libraryManagementTestFixtures';

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

describe('LibraryManagementRootAssignment', () => {
	it('warns on a library whose disk cannot add files safely and offers the check', async () => {
		h.placement.data = {
			roots: [
				{ root_id: 'root-1', mode: 'check' },
				{ root_id: 'root-2', mode: 'unknown' },
				{ root_id: 'root-3', mode: 'link' }
			]
		};
		await render(SettingsLibraryManagement, {
			roots: [
				roots[0],
				{ ...roots[0], id: 'root-2', label: 'Vinyl rips', path: '/music/vinyl' },
				{ ...roots[0], id: 'root-3', label: 'Singles', path: '/music/singles' }
			],
			policyRevision: 'policy-1'
		});

		await expect.element(page.getByText('Files can be overwritten in this library')).toBeVisible();
		await expect
			.element(
				page.getByText(
					'The disk under "Archive" has no way to add a file only when its name is free.'
				)
			)
			.toBeVisible();
		await expect.element(page.getByText('Not checked yet')).toBeVisible();
		// The warned library and the unchecked one offer the check; the safe one shows nothing.
		const checks = page.getByRole('button', { name: 'Check again' });
		await expect.element(checks.nth(1)).toBeVisible();
		expect(checks.elements()).toHaveLength(2);
		await checks.nth(1).click();
		expect(h.checkPlacement).toHaveBeenCalledWith('root-2');
	});

	it('round-trips the explicit per-root multi-disc script mode', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByText('Per-root profile overrides').click();
		await page.getByRole('checkbox', { name: /Override selected profile values/ }).click();

		const mode = page.getByRole('combobox', { name: 'Multi-disc naming' });
		await expect.element(mode).toHaveValue('inherit');
		await expect
			.element(page.getByRole('combobox', { name: 'Selected multi-disc script' }))
			.not.toBeInTheDocument();
		await mode.selectOptions('script');
		const selectedScript = page.getByRole('combobox', { name: 'Selected multi-disc script' });
		await expect.element(selectedScript).toBeVisible();
		await selectedScript.selectOptions(multiDiscNamingScriptId);
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					root_assignments: [
						expect.objectContaining({
							overrides: expect.objectContaining({
								multi_disc_naming_mode: 'script',
								multi_disc_naming_script_id: multiDiscNamingScriptId
							})
						})
					]
				})
			})
		);
	});

	it('serializes the per-root effective-standard mode without a script ID', async () => {
		await render(SettingsLibraryManagement, { roots, policyRevision: 'policy-1' });
		await page.getByRole('checkbox', { name: /Configure file organization/ }).click();
		await page.getByText('Per-root profile overrides').click();
		await page.getByRole('checkbox', { name: /Override selected profile values/ }).click();
		await page.getByRole('combobox', { name: 'Multi-disc naming' }).selectOptions('standard');
		await page.getByRole('button', { name: 'Validate and save' }).click();

		expect(h.update).toHaveBeenCalledWith(
			expect.objectContaining({
				settings: expect.objectContaining({
					root_assignments: [
						expect.objectContaining({
							overrides: expect.objectContaining({
								multi_disc_naming_mode: 'standard',
								multi_disc_naming_script_id: null
							})
						})
					]
				})
			})
		);
	});
});
