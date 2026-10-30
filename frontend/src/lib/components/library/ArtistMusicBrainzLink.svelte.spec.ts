import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import ArtistMusicBrainzLink from './ArtistMusicBrainzLink.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

const ROUTE = '/api/v3/library/artists/{id}/musicbrainz-link';
const OLD = 'aaaaaaaa-0000-4000-8000-00000000000a';
const NEW = 'aaaaaaaa-0000-4000-8000-00000000000c';

const appLink: RouteData<`GET ${typeof ROUTE}`> = {
	artist_id: 'artist-a',
	artist_revision: 3,
	musicbrainz_artist_id: OLD,
	source: 'app',
	set_by: 'app',
	selected_at: 1
};

function signIn(role: 'user' | 'trusted') {
	authStore.setUser(
		toAuthUser({
			id: 'user-1',
			display_name: 'Someone',
			role,
			email: null,
			avatar_url: null,
			username: 'someone',
			username_display: 'Someone'
		})
	);
}

beforeEach(() => {
	localStorage.clear();
});

describe('artist MusicBrainz link', () => {
	it('lets a curator link the artist by hand, from a page address', async () => {
		signIn('trusted');
		const api = fakeApi({
			[`GET ${ROUTE}`]: appLink,
			[`PUT ${ROUTE}`]: {
				...appLink,
				artist_revision: 4,
				musicbrainz_artist_id: NEW,
				source: 'person',
				set_by: 'you'
			}
		});
		await renderWithQuery(ArtistMusicBrainzLink, { artistId: 'artist-a' });

		await expect.element(page.getByText(/Linked by DroppedNeedle/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Clear' })).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Change' }).click();
		await page
			.getByRole('textbox', { name: 'MusicBrainz artist ID or page address' })
			.fill(`https://musicbrainz.org/artist/${NEW}`);
		await page.getByRole('button', { name: 'Link', exact: true }).click();

		await vi.waitFor(() => expect(api.calls(`PUT ${ROUTE}`)).toHaveLength(1));
		expect(api.calls(`PUT ${ROUTE}`)[0].body).toEqual({
			musicbrainz_artist_id: NEW,
			expected_revision: 3
		});
	});

	it('names the other artists when a link would join them', async () => {
		signIn('trusted');
		fakeApi({
			[`GET ${ROUTE}`]: appLink,
			[`PUT ${ROUTE}`]: apiError(
				409,
				'ARTIST_LINK_SHARED',
				'Other artists in the library are this MusicBrainz artist too.',
				{
					action: 'An admin can merge them in Duplicate artists.',
					artists: [{ id: 'artist-b', name: 'Beta' }]
				}
			)
		});
		await renderWithQuery(ArtistMusicBrainzLink, { artistId: 'artist-a' });

		await page.getByRole('button', { name: 'Change' }).click();
		await page.getByRole('textbox', { name: 'MusicBrainz artist ID or page address' }).fill(NEW);
		await page.getByRole('button', { name: 'Link', exact: true }).click();

		await expect.element(page.getByRole('alert')).toHaveTextContent('(Beta)');
	});

	it('shows a plain user who set the link, with no controls', async () => {
		signIn('user');
		fakeApi({
			[`GET ${ROUTE}`]: { ...appLink, source: 'person', set_by: 'person' }
		});
		await renderWithQuery(ArtistMusicBrainzLink, { artistId: 'artist-a' });

		await expect.element(page.getByText(/Linked by a curator/)).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Change' })).not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Clear' })).not.toBeInTheDocument();
	});
});
