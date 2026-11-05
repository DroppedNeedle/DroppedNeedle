import { page, userEvent } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/state', async () => ({ page: (await import('$lib/test/page.svelte')).testPage }));
vi.mock('$app/navigation', async () => ({
	goto: (await import('$lib/test/page.svelte')).testGoto
}));

import TracksPage from './+page.svelte';
import { testPage } from '$lib/test/page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

type TrackPage = RouteData<'GET /api/v3/library/tracks'>;

function track(id: string, title: string, artist: string): TrackPage['items'][number] {
	return {
		id,
		title,
		album_id: `album-${id}`,
		row_revision: 1,
		album_title: `Album ${id}`,
		artist_id: null,
		artist_name: artist,
		album_artist_id: null,
		album_artist_name: artist,
		disc_number: 1,
		track_number: 1,
		year: null,
		genre: null,
		duration_seconds: 200,
		format: 'flac',
		bit_rate: null,
		sample_rate: null,
		bit_depth: null,
		channels: null,
		file_size_bytes: 1024,
		date_added: 1,
		cover_available: false,
		art_missing: false,
		favorite: false
	};
}

// The track list's format counts come from the library totals.
const stats: RouteData<'GET /api/v3/library/stats'> = {
	total_albums: 1,
	total_artists: 1,
	total_tracks: 1,
	total_size_bytes: 1,
	format_breakdown: { FLAC: 1 },
	favorite_albums: 0,
	favorite_artists: 0,
	favorite_tracks: 0,
	check_count: 0,
	guess_count: 0,
	local_only_count: 0,
	looks_wrong_count: 0,
	review_count: 0,
	not_found_count: 0
};

beforeEach(() => {
	localStorage.clear();
	testPage.url = new URL('http://localhost/library/tracks');
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

describe('library tracks page', () => {
	it('sorts by artist and loads more after the last page by its cursor', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': ({ query }): TrackPage =>
				query.get('after') === 'page-2'
					? { items: [track('t2', 'Second Song', 'Boreal')], total: 2, next: null }
					: { items: [track('t1', 'First Song', 'Aurora')], total: 2, next: 'page-2' },
			'GET /api/v3/library/stats': stats
		});
		await renderWithQuery(TracksPage, {});

		await expect.element(page.getByText('First Song')).toBeVisible();
		await userEvent.selectOptions(page.getByRole('combobox', { name: 'Sort tracks' }), 'artist');
		await expect
			.poll(() => api.calls('GET /api/v3/library/tracks').at(-1)?.query.get('sort'))
			.toBe('artist');
		const sorted = api.calls('GET /api/v3/library/tracks').at(-1);
		expect(sorted?.query.get('order')).toBe('asc');
		expect(sorted?.query.has('after')).toBe(false);

		await page.getByRole('button', { name: 'Load more (1 / 2)' }).click();
		await expect.element(page.getByText('Second Song')).toBeVisible();
		const more = api.calls('GET /api/v3/library/tracks').at(-1);
		expect(more?.query.get('after')).toBe('page-2');
		expect(more?.query.get('sort')).toBe('artist');
		await expect.element(page.getByRole('button', { name: /Load more/ })).not.toBeInTheDocument();
	});

	it('sorts by album, A to Z', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': {
				items: [track('t1', 'First Song', 'Aurora')],
				total: 1,
				next: null
			},
			'GET /api/v3/library/stats': stats
		});
		await renderWithQuery(TracksPage, {});

		await expect.element(page.getByText('First Song')).toBeVisible();
		await userEvent.selectOptions(page.getByRole('combobox', { name: 'Sort tracks' }), 'album');
		await expect
			.poll(() => api.calls('GET /api/v3/library/tracks').at(-1)?.query.get('sort'))
			.toBe('album');
		expect(api.calls('GET /api/v3/library/tracks').at(-1)?.query.get('order')).toBe('asc');
	});

	it('sends the format and rating filters, and keeps them for the next visit', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': (): TrackPage => ({
				items: [track('t1', 'First Song', 'Aurora')],
				total: 1,
				next: null
			}),
			'GET /api/v3/library/stats': stats
		});
		testPage.url = new URL(
			'http://localhost/library/tracks?formats=flac&format_mode=without&rating=4&rating_mode=at_most'
		);
		await renderWithQuery(TracksPage, {});

		await expect.element(page.getByText('First Song')).toBeVisible();
		const sent = api.calls('GET /api/v3/library/tracks').at(-1)?.query;
		expect(sent?.get('formats')).toBe('flac');
		expect(sent?.get('format_mode')).toBe('without');
		expect(sent?.get('rating')).toBe('4');
		expect(sent?.get('rating_mode')).toBe('at_most');
		expect(JSON.parse(localStorage.getItem('library.filters.tracks') ?? '{}')).toEqual({
			formats: 'flac',
			format_mode: 'without',
			rating: '4',
			rating_mode: 'at_most'
		});
	});
});
