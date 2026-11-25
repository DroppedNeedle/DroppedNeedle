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
	it('sorts by artist, reading the list from its first position', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': {
				items: [track('t1', 'First Song', 'Aurora')],
				total: 1,
				next: null
			},
			'GET /api/v3/library/tracks/letters': { letters: [{ key: 'A', position: 0, count: 1 }] },
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
		expect(sorted?.query.get('offset')).toBe('0');
		expect(sorted?.query.has('after')).toBe(false);
		await expect
			.poll(() => api.calls('GET /api/v3/library/tracks/letters').at(-1)?.query.get('sort'))
			.toBe('artist');
	});

	it('draws 50,000 tracks as one list, mounting only the rows near the screen', async () => {
		fakeApi({
			'GET /api/v3/library/tracks': ({ query }): TrackPage => {
				const offset = Number(query.get('offset') ?? 0);
				const count = Math.max(0, Math.min(Number(query.get('limit')), 50000 - offset));
				return {
					items: Array.from({ length: count }, (_, i) =>
						track(`t${offset + i}`, `Track ${offset + i}`, 'Aurora')
					),
					total: 50000,
					next: null
				};
			},
			'GET /api/v3/library/stats': stats
		});
		await renderWithQuery(TracksPage, {});
		const list = page.getByRole('table', { name: 'All tracks' });
		await expect.element(list).toHaveAttribute('aria-rowcount', '50000');

		const box = list.element().parentElement as HTMLElement;
		window.scrollTo(0, box.getBoundingClientRect().top + window.scrollY + box.offsetHeight * 0.6);
		// The rows there load and paint on a later frame.
		await expect
			.poll(() => document.querySelector('[aria-label="Play Track 30000"]'), {
				timeout: 5000,
				interval: 100
			})
			.not.toBeNull();

		const rows = [...list.element().querySelectorAll('[role="row"]')];
		// Two screens of rows: the one in view and half a screen either side.
		expect(rows.length).toBeLessThanOrEqual(Math.ceil((window.innerHeight * 2) / 64) + 2);
		expect(list.element().querySelectorAll('*').length).toBeLessThan(5000);
		const onScreen = rows.filter((row) => {
			const rect = row.getBoundingClientRect();
			return rect.top >= 0 && rect.bottom <= window.innerHeight;
		});
		expect(onScreen.length).toBeGreaterThan(0);
		for (const row of onScreen) {
			const position = Number(row.getAttribute('aria-rowindex')) - 1;
			expect(Math.abs(position - 30000)).toBeLessThan(window.innerHeight / 32);
			expect(row.textContent).toContain(`Track ${position}`);
		}
		window.scrollTo(0, 0);
	});

	it('plays from the pressed row by its position in the list', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': ({ query }): TrackPage => {
				const offset = Number(query.get('offset') ?? 0);
				return {
					items: Array.from({ length: 3 }, (_, i) =>
						track(`t${offset + i}`, `Track ${offset + i}`, 'Aurora')
					),
					total: 3,
					next: null
				};
			},
			'GET /api/v3/library/stats': stats
		});
		await renderWithQuery(TracksPage, {});

		await page.getByRole('button', { name: 'Play Track 2' }).click();
		await expect
			.poll(() =>
				api.calls('GET /api/v3/library/tracks').some((call) => call.query.get('offset') === '2')
			)
			.toBe(true);
		const started = api
			.calls('GET /api/v3/library/tracks')
			.find((call) => call.query.get('offset') === '2');
		expect(started?.query.get('limit')).toBe('500');
	});

	it('sorts by album, A to Z', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': {
				items: [track('t1', 'First Song', 'Aurora')],
				total: 1,
				next: null
			},
			'GET /api/v3/library/tracks/letters': { letters: [] },
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
		expect(JSON.parse(localStorage.getItem('library.filters.user-1.tracks') ?? '{}')).toEqual({
			formats: 'flac',
			format_mode: 'without',
			rating: '4',
			rating_mode: 'at_most'
		});
	});
});
