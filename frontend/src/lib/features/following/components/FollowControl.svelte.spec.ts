import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

import FollowControl from './FollowControl.svelte';

const ARTIST = '10adbe5e-0000-4000-8000-000000000071';

beforeEach(() => {
	localStorage.clear();
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'Ada'
		})
	);
});

describe('following an artist', () => {
	it("sends the artist's name with the follow", async () => {
		const api = fakeApi({
			'GET /api/v3/artists/{artist_mbid}/follow-status': {
				artist_mbid: ARTIST,
				followed: false,
				auto_download: false,
				auto_download_state: 'none'
			},
			'PUT /api/v3/artists/{artist_mbid}/follow': {
				artist_mbid: ARTIST,
				followed: true,
				auto_download: false,
				auto_download_state: 'none'
			}
		});
		// The follow writes through the app's own query client.
		await renderWithQuery(
			FollowControl,
			{ artistMbid: ARTIST, artistName: 'Massive Attack' },
			{ client: queryClient }
		);

		await page.getByRole('button', { name: 'Follow this artist' }).click();

		await expect.poll(() => api.calls('PUT /api/v3/artists/{artist_mbid}/follow').length).toBe(1);
		const [call] = api.calls('PUT /api/v3/artists/{artist_mbid}/follow');
		expect(call?.params).toEqual({ artist_mbid: ARTIST });
		expect(call?.body).toEqual({ followed: true, artist_name: 'Massive Attack' });
	});
});
