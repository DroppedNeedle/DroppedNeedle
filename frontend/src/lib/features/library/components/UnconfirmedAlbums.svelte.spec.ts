import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

import UnconfirmedAlbums from './UnconfirmedAlbums.svelte';

afterEach(() => authStore.clear());

type Waiting = RouteData<'GET /api/v3/library/unconfirmed'>['items'][number];

function album(id: string, title: string, code: string, message: string): Waiting {
	return {
		album_id: id,
		title,
		artist_name: 'The Lanterns',
		state: 'unmatched',
		release_mbid: null,
		release_group_mbid: null,
		reason: { code, message, action: '' },
		updated_at: 1,
		identity_revision: 0,
		guess: null,
		file_count: 3,
		looks_wrong_count: 0
	};
}

describe('Check albums', () => {
	it('lists an edition taken anyway as needing its files matched, not as not found', async () => {
		authStore.setUser(
			toAuthUser({
				id: 'user-1',
				display_name: 'Curator',
				role: 'trusted',
				email: null,
				avatar_url: null,
				username: 'curator',
				username_display: 'Curator'
			})
		);
		fakeApi({
			'GET /api/v3/library/unconfirmed': {
				items: [
					album(
						'album-1',
						'Track Album',
						'CHOSEN_EDITION_FITS_NO_FILE',
						"The edition you chose doesn't match these files."
					),
					album('album-2', 'Lost Album', 'NO_CANDIDATE', 'Nothing on MusicBrainz looks like it.')
				],
				next_cursor: null,
				total: 2
			}
		});

		await renderWithQuery(UnconfirmedAlbums, { view: 'unmatched' });

		const row = (title: string) => page.getByRole('listitem').filter({ hasText: title });
		await expect.element(row('Track Album').getByText('Needs matching')).toBeVisible();
		await expect
			.element(row('Track Album').getByRole('link', { name: 'Match files' }))
			.toBeVisible();
		await expect.element(row('Track Album').getByText('Not found')).not.toBeInTheDocument();
		await expect.element(row('Lost Album').getByText('Not found')).toBeVisible();
		await expect
			.element(row('Lost Album').getByRole('link', { name: 'Pick an edition' }))
			.toBeVisible();
	});
});
