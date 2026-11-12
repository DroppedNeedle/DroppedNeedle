import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import RecycleBinFolderAccept from './RecycleBinFolderAccept.svelte';

const ROUTE = 'POST /api/v3/library/recycle-bin/accept-folder';
const PATH = '/mnt/nas/music-recycle';

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'Admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'Admin'
		})
	);
});

describe('using a folder as the recycle bin', () => {
	it('asks first, then sends the saved path', async () => {
		const api = fakeApi({ [ROUTE]: undefined });
		await renderWithQuery(RecycleBinFolderAccept, { path: PATH });

		await page.getByRole('button', { name: 'Use this folder' }).click();
		const dialog = page.getByRole('dialog', { name: 'Use this folder as the recycle bin?' });
		await expect.element(dialog.getByText(PATH)).toBeVisible();
		expect(api.calls(ROUTE)).toHaveLength(0);

		await dialog.getByRole('button', { name: 'Use this folder' }).click();
		await expect.element(dialog).not.toBeInTheDocument();
		const [call] = api.calls(ROUTE);
		expect(call?.body).toEqual({ path: PATH });
	});

	it('says why the server refused', async () => {
		const refused = 'The recycle bin path changed, so nothing was marked.';
		fakeApi({ [ROUTE]: apiError(409, 'STALE_REVISION', refused) });
		await renderWithQuery(RecycleBinFolderAccept, { path: PATH });

		await page.getByRole('button', { name: 'Use this folder' }).click();
		const dialog = page.getByRole('dialog', { name: 'Use this folder as the recycle bin?' });
		await dialog.getByRole('button', { name: 'Use this folder' }).click();
		await expect.element(dialog.getByText(refused)).toBeVisible();
	});
});
