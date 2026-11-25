import { page } from 'vitest/browser';
import { expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import SmartPlaylistShelf from './SmartPlaylistShelf.svelte';

it('saves a named rating rule and links to its matching songs', async () => {
	authStore.setUser(
		toAuthUser({
			id: 'owner',
			display_name: 'Owner',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'owner',
			username_display: 'Owner'
		})
	);
	const lists: RouteData<'GET /api/v3/smart-playlists'> = [];
	const api = fakeApi({
		'GET /api/v3/smart-playlists': () => lists,
		'POST /api/v3/smart-playlists': () => {
			const saved = { id: 'five-stars', name: 'Keepers', min_rating: 5 };
			lists.push(saved);
			return saved;
		}
	});
	await renderWithQuery(SmartPlaylistShelf, {});
	await page.getByRole('button', { name: 'New smart playlist' }).click();
	await page.getByRole('textbox', { name: 'Playlist name' }).fill('  Keepers  ');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect
		.element(page.getByRole('link', { name: 'Keepers' }))
		.toHaveAttribute('href', '/playlists/smart/five-stars');
	expect(api.calls('POST /api/v3/smart-playlists')[0]?.body).toEqual({
		name: 'Keepers',
		min_rating: 5
	});
});
