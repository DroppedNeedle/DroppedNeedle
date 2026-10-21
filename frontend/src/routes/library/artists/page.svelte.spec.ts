import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/library/artists?view=contributors') }
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
// Keep the image on its first source: the test has no server to answer it.
vi.mock('$lib/utils/lazyImage', () => ({
	lazyImage: () => ({ destroy() {} }),
	resetLazyImage: () => {}
}));

import ArtistsPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const ARTIST_MBID = '6c5bfdbe-6325-4897-aaa5-f0ab06bdc799';

const contributors: RouteData<'GET /api/v3/library/artists'> = {
	items: [
		{
			id: '659cab1f-92ab-536b-9398-96a4ea0a4a90',
			name: 'A Day to Remember',
			artist_mbid: ARTIST_MBID,
			identity_state: 'linked',
			album_count: 0,
			track_count: 5,
			appearance_album_count: 2,
			appearance_track_count: 3,
			date_added: 1,
			favorite: false
		}
	],
	total: 1,
	album_artist_total: 0,
	contributor_total: 1,
	next: null
};

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
});

describe('library artists page, contributors view', () => {
	it("shows a linked contributor's picture, link and appearance counts", async () => {
		const api = fakeApi({ 'GET /api/v3/library/artists': contributors });
		await renderWithQuery(ArtistsPage, {});

		await expect
			.element(page.getByRole('img', { name: 'A Day to Remember' }))
			.toHaveAttribute('data-src', `/api/v3/covers/artist/${ARTIST_MBID}?size=250`);
		await expect
			.element(page.getByRole('link', { name: 'Open A Day to Remember' }))
			.toHaveAttribute('href', `/artist/${ARTIST_MBID}`);
		await expect.element(page.getByText('2 releases · 3 appearances')).toBeVisible();
		expect(api.calls('GET /api/v3/library/artists')[0].query.get('scope')).toBe('contributors');
	});
});
