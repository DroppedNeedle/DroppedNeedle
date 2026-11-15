import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import DropImportMatchModal from './DropImportMatchModal.svelte';

const found: RouteData<'GET /api/v3/search/{bucket}'> = {
	bucket: 'albums',
	limit: 20,
	offset: 0,
	status: 'ok',
	results: [
		{
			kind: 'album',
			title: 'Dummy',
			artist: 'Portishead',
			year: 1994,
			musicbrainz_id: '76df3287-6cda-33eb-8e9a-044b5e15ffdd',
			in_library: false,
			requested: false,
			score: 95,
			disambiguation: null,
			type_info: null,
			art_missing: true
		}
	]
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'curator-1',
			display_name: 'Curator',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'curator',
			username_display: 'Curator'
		})
	);
});

describe('matching a held entry', () => {
	it('names the refused file inside the modal, and clears the old failure on the next pick', async () => {
		const refused = [
			'Cannot read /music/Odds/01 Glory Box.flac: unsupported codec',
			'Cannot read /music/Odds/02 Glory Box.flac: unsupported codec'
		];
		let attempt = 0;
		const api = fakeApi({
			'GET /api/v3/search/{bucket}': found,
			'POST /api/v3/held/{entry_id}/match': () =>
				apiError(422, 'VALIDATION_FAILED', refused[attempt++])
		});
		const closed = vi.fn();
		await renderWithQuery(DropImportMatchModal, {
			entryId: 12,
			name: 'Odds',
			search: 'Glory Box',
			fileCount: 2,
			onclose: closed
		});

		const dialog = page.getByRole('dialog', { name: 'Match to an album' });
		const pick = dialog.getByRole('button', { name: /Dummy/ });
		await expect.element(pick).toBeVisible();
		await pick.click();

		const alert = dialog.getByRole('alert');
		await expect.element(alert).toHaveTextContent(refused[0]);
		await expect.element(dialog).toBeVisible();
		expect(api.calls('POST /api/v3/held/{entry_id}/match')).toHaveLength(1);
		expect(closed).not.toHaveBeenCalled();

		await pick.click();
		await expect.element(alert).toHaveTextContent(refused[1]);
		await expect.element(alert).not.toHaveTextContent(refused[0]);
		await expect.element(dialog).toBeVisible();
		expect(closed).not.toHaveBeenCalled();
	});
});
