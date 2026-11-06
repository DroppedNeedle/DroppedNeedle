import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import ImportFolderLine from './ImportFolderLine.svelte';

function signIn(role: 'admin' | 'trusted') {
	authStore.setUser(
		toAuthUser({
			id: `${role}-1`,
			display_name: role,
			role,
			email: null,
			avatar_url: null,
			username: role,
			username_display: role
		})
	);
}

describe('the import folder line on the Downloads page', () => {
	it('points an admin with no folder at the setting', async () => {
		signIn('admin');
		fakeApi({
			'GET /api/v3/settings/library/import-folder': { path: '' },
			'GET /api/v3/settings/library/import-folder/status': {
				state: 'off',
				overlap: null,
				overlap_path: null,
				waiting: 0
			}
		});
		await renderWithQuery(ImportFolderLine, {});

		await expect.element(page.getByText('Bringing in a whole library?')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Set an import folder' }))
			.toHaveAttribute('href', expect.stringContaining('tab=library#import-folder'));
	});

	it('shows the folder and what it is doing', async () => {
		signIn('admin');
		fakeApi({
			'GET /api/v3/settings/library/import-folder': { path: '/data/import' },
			'GET /api/v3/settings/library/import-folder/status': {
				state: 'importing',
				overlap: null,
				overlap_path: null,
				waiting: 12
			}
		});
		await renderWithQuery(ImportFolderLine, {});

		await expect.element(page.getByText('/data/import')).toBeVisible();
		await expect.element(page.getByText('Importing · 12 albums left')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Settings' })).toBeVisible();
	});

	it('shows nothing to anyone else', async () => {
		signIn('trusted');
		fakeApi({});
		const screen = await renderWithQuery(ImportFolderLine, {});

		await expect.element(page.getByText('Import folder')).not.toBeInTheDocument();
		expect(screen.container.textContent?.trim()).toBe('');
	});
});
