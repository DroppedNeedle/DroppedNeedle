import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import { resetPreviewMocks } from './libraryManagementPreviewTestFixtures';

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

describe('LibraryManagementCollisionDialog', () => {
	it('never preselects a collision action and disables recycling without a configured path', async () => {
		await render(LibraryManagementPreviewPage, { jobId: 'preview-1' });
		await page.getByText('Inspect exact diff').click();
		await page.getByRole('button', { name: 'Choose resolution...' }).click();

		await expect
			.element(page.getByRole('heading', { name: 'Choose a collision resolution' }))
			.toHaveFocus();
		await expect.element(page.getByRole('radio', { name: /Keep existing/ })).not.toBeChecked();
		await expect
			.element(page.getByRole('radio', { name: /Keep incoming at an alternate/ }))
			.not.toBeChecked();
		await expect.element(page.getByRole('radio', { name: /Recycle existing/ })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Generate resolution preview' }))
			.toBeDisabled();
	});
});
