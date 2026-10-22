import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';
import type { RequestItem } from '$lib/queries/requests/types';

import RequestCard from './RequestCard.svelte';

const RECORDING = '6d0b7a1e-0000-4000-8000-000000000028';

const held: RequestItem = {
	musicbrainz_id: RECORDING,
	artist_name: 'Portishead',
	album_title: 'Glory Box',
	requested_at: 1_700_000_000,
	completed_at: 1_700_000_100,
	status: 'failed',
	request_kind: 'track',
	origin: 'user',
	track_title: 'Glory Box',
	user_id: 'user-1',
	requester_count: 0,
	album_version_only: true
};

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

describe('a request card for a single found only on its album', () => {
	it('says why and takes the song from the album in one click', async () => {
		const api = fakeApi({
			'POST /api/v3/requests/tracks/{musicbrainz_id}/from-album': {
				success: true,
				message: 'Re-requested Glory Box'
			}
		});
		await renderWithQuery(RequestCard, { item: held, mode: 'history', onretry: () => {} });

		await expect.element(page.getByText('Only the album version was found')).toBeVisible();
		await page.getByRole('button', { name: 'Get it from the album' }).click();

		await expect
			.poll(() => api.calls('POST /api/v3/requests/tracks/{musicbrainz_id}/from-album').length)
			.toBe(1);
		const [call] = api.calls('POST /api/v3/requests/tracks/{musicbrainz_id}/from-album');
		expect(call?.params).toEqual({ musicbrainz_id: RECORDING });
	});

	it('offers nothing when the request failed for another reason', async () => {
		fakeApi({});
		await renderWithQuery(RequestCard, {
			item: { ...held, album_version_only: false },
			mode: 'history',
			onretry: () => {}
		});

		await expect.element(page.getByRole('button', { name: 'Retry request' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Get it from the album' }))
			.not.toBeInTheDocument();
	});
});
