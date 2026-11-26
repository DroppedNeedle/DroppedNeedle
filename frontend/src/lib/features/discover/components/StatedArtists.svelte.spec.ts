import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import StatedArtists from './StatedArtists.svelte';

const STATED = 'GET /api/v3/discover/stated-artists';
const NAME = 'PUT /api/v3/discover/stated-artists/{artist_mbid}';
const REMOVE = 'DELETE /api/v3/discover/stated-artists/{artist_mbid}';
const SUGGEST = 'GET /api/v3/search/suggest';
type Stated = RouteData<typeof STATED>['items'][number];
type Suggestion = RouteData<typeof SUGGEST>['results'][number];

const CIRCA = '11111111-1111-4111-8111-111111111111';
const SAOSIN = '22222222-2222-4222-8222-222222222222';

function suggestion(over: Partial<Suggestion>): Suggestion {
	return { kind: 'artist', title: '', in_library: false, requested: false, score: 90, ...over };
}

const FOUND: Suggestion[] = [
	suggestion({ title: 'Circa Survive', musicbrainz_id: CIRCA, in_library: true }),
	suggestion({ title: 'Juturna', kind: 'album', musicbrainz_id: 'rg-1', artist: 'Circa Survive' }),
	suggestion({ title: 'Saosin', musicbrainz_id: SAOSIN, disambiguation: 'US post-hardcore band' })
];

function signIn() {
	authStore.setUser(
		toAuthUser({
			id: 'u1',
			display_name: 'Ada',
			role: 'user',
			email: null,
			avatar_url: null,
			username: 'ada',
			username_display: 'ada'
		})
	);
}

describe('StatedArtists.svelte', () => {
	it('lists artists as you type, names one with Add, and its tag removes it', async () => {
		let items: Stated[] = [];
		const api = fakeApi({
			[STATED]: () => ({ items, limit: 30 }),
			[SUGGEST]: { results: FOUND, status: 'ok' },
			[NAME]: ({ params, body }) => {
				const named = {
					artist_mbid: params.artist_mbid,
					artist_name: (body as { artist_name: string }).artist_name,
					stated_at: 1
				};
				items = [...items, named];
				return named;
			},
			[REMOVE]: ({ params }) => {
				items = items.filter((artist) => artist.artist_mbid !== params.artist_mbid);
			}
		});
		signIn();
		// The mutations' effects sweep the app's own client.
		await renderWithQuery(StatedArtists, { first: true }, { client: queryClient });

		await expect.element(page.getByText('0 of 3 for a first set')).toBeVisible();
		await page.getByRole('searchbox', { name: 'Name an artist you love' }).fill('circa');
		await expect.element(page.getByText('In your library')).toBeVisible();
		await expect.element(page.getByText('US post-hardcore band')).toBeVisible();
		await expect.element(page.getByText('Juturna')).not.toBeInTheDocument();

		await page.getByRole('button', { name: 'Add Circa Survive' }).click();
		await expect.element(page.getByRole('button', { name: 'Remove Circa Survive' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Circa Survive added' })).toBeDisabled();
		await expect.element(page.getByText('1 of 3 for a first set')).toBeVisible();
		expect(api.calls(NAME).map((call) => [call.params.artist_mbid, call.body])).toEqual([
			[CIRCA, { artist_name: 'Circa Survive' }]
		]);

		await page.getByRole('button', { name: 'Remove Circa Survive' }).click();
		await expect
			.element(page.getByRole('button', { name: 'Remove Circa Survive' }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Add Circa Survive' })).toBeEnabled();
		expect(api.calls(REMOVE).map((call) => call.params.artist_mbid)).toEqual([CIRCA]);
	});

	it('at the limit, the field says so and takes no more', async () => {
		const items: Stated[] = Array.from({ length: 30 }, (_, n) => ({
			artist_mbid: `${String(n).padStart(8, '0')}-1111-4111-8111-111111111111`,
			artist_name: `Artist ${n}`,
			stated_at: n
		}));
		fakeApi({ [STATED]: { items, limit: 30 } });
		signIn();
		await renderWithQuery(StatedArtists, {});

		await expect
			.element(page.getByText('That is 30, the most. Remove one to name another.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('searchbox', { name: 'Name an artist you love' }))
			.toBeDisabled();
	});
});
