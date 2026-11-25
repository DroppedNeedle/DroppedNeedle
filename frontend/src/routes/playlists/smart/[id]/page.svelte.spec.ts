import { page } from 'vitest/browser';
import { expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';
import SmartPlaylistPage from './+page.svelte';

it('reads the next page and starts from the first match when the saved rule changes', async () => {
	authStore.setUser(
		toAuthUser({
			id: 'owner',
			role: 'user',
			display_name: 'Owner',
			email: null,
			avatar_url: null,
			username: 'owner',
			username_display: 'Owner'
		})
	);
	let minRating = 5;
	const definition = () => ({ id: 'ratings', name: 'Keepers', min_rating: minRating });
	const track = (
		title: string
	): RouteData<'GET /api/v3/smart-playlists/{playlist_id}/tracks'>['items'][number] => ({
		id: title,
		title,
		album_id: 'album',
		album_title: 'Collection',
		artist_name: 'Quartet',
		album_artist_name: 'Quartet',
		art_missing: true,
		cover_available: false,
		favorite: false,
		disc_number: 1,
		track_number: 1,
		file_size_bytes: 1024,
		format: 'flac',
		row_revision: 1
	});
	const api = fakeApi({
		'GET /api/v3/smart-playlists/{playlist_id}': definition,
		'GET /api/v3/smart-playlists/{playlist_id}/tracks': ({ query }) => {
			if (query.get('after') === 'next') return { items: [track('Side B')], total: 2 };
			return { items: [track(minRating === 5 ? 'Side A' : 'Four stars')], next: 'next', total: 2 };
		},
		'PUT /api/v3/smart-playlists/{playlist_id}': ({ body }) => {
			minRating = (body as { min_rating: number }).min_rating;
			return definition();
		}
	});
	const props = {
		params: { id: 'ratings' },
		data: { primarySource: 'listenbrainz' as const, user: authStore.user }
	};
	const view = await renderWithQuery(SmartPlaylistPage, props);
	await expect.element(page.getByText('Side A', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Next' }).click();
	await expect.element(page.getByText('Side B', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Edit rule' }).click();
	await page.getByRole('combobox', { name: 'My rating is at least' }).selectOptions('4');
	await page.getByRole('button', { name: 'Save' }).click();
	await expect.element(page.getByText('Four stars', { exact: true })).toBeVisible();
	await expect.element(page.getByRole('button', { name: 'Previous' })).toBeDisabled();
	expect(api.calls('PUT /api/v3/smart-playlists/{playlist_id}')[0]?.body).toEqual({
		name: 'Keepers',
		min_rating: 4
	});

	// Another signed-in client changes the rule while this page is closed.
	await view.unmount();
	minRating = 3;
	await renderWithQuery(SmartPlaylistPage, props, { client: view.client });
	await expect.element(page.getByText('My rating is at least 3 stars')).toBeVisible();
});
