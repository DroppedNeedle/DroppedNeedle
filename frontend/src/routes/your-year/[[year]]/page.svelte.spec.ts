import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import YearPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

type Review = RouteData<'GET /api/v3/me/year-in-review/{year}'>;
type Years = RouteData<'GET /api/v3/me/year-in-review'>;

const albums = Array.from({ length: 10 }, (_, i) => ({
	id: `album-${i + 1}`,
	name: `Album ${i + 1}`,
	artist_name: `Artist ${i + 1}`,
	plays: 100 - i * 5,
	release_group_mbid: null,
	cover_available: false,
	art_missing: true
}));
const artists = Array.from({ length: 10 }, (_, i) => ({
	id: `artist-${i + 1}`,
	mbid: null,
	name: `Artist ${i + 1}`,
	plays: 300 - i * 10,
	art_missing: true
}));
const songs = Array.from({ length: 10 }, (_, i) => ({
	id: `track-${i + 1}`,
	album_id: `album-${i + 1}`,
	album_name: `Album ${i + 1}`,
	release_group_mbid: null,
	cover_available: false,
	name: `Song ${i + 1}`,
	artist_name: `Artist ${i + 1}`,
	plays: 40 - i,
	art_missing: true
}));

function fullYear(): Review {
	return {
		year: 2025,
		zone: 'Europe/London',
		state: 'complete',
		first_day: '2025-01-01',
		last_day: '2025-12-31',
		plays: 900,
		artists: 61,
		days_played: 291,
		listened_ms: 118 * 3_600_000 + 1_000,
		plays_without_length: 0,
		top_albums: albums,
		top_artists: artists,
		top_songs: songs,
		months: [10, 20, 30, 40, 50, 260, 70, 80, 90, 100, 110, 40],
		busiest_month: { month: 6, plays: 260, album: { ...albums[0], plays: 41 } },
		first_play: { played_at_ms: Date.UTC(2025, 0, 1, 0, 14), song: 'Song 1', artist: 'Artist 1' },
		busiest_day: { date: '2025-06-14', plays: 64 },
		longest_streak: { days: 23, first_day: '2025-03-01', last_day: '2025-03-23' },
		new_artists: {
			count: 19,
			artists: [artists[3], artists[4]],
			first: { artist: artists[3], date: '2025-02-03' }
		}
	};
}

const years: Years = {
	zone: 'Europe/London',
	years: [
		{ year: 2026, plays: 12, state: 'too_few_plays' },
		{ year: 2025, plays: 900, state: 'complete' }
	]
};

function serve(review: Review) {
	fakeApi({
		'GET /api/v3/me/year-in-review': years,
		'GET /api/v3/me/year-in-review/{year}': review
	});
}

async function open(year: string) {
	await renderWithQuery(YearPage, {
		params: { year },
		data: { user: null, primarySource: 'listenbrainz' }
	});
}

async function regionNames(): Promise<string[]> {
	return (await page.getByRole('region').all()).map(
		(region) => region.element().getAttribute('aria-label') ?? ''
	);
}

beforeEach(() => {
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

describe('a year in review', () => {
	it('shows a full year section by section, with the No. 1 album and each rank and play count', async () => {
		serve(fullYear());
		await open('2025');

		const hero = page.getByRole('region', { name: 'Your year' });
		await expect.element(hero.getByRole('heading', { level: 1, name: '2025' })).toBeVisible();
		await expect.element(hero.getByText('No. 1 album')).toBeVisible();
		await expect.element(hero.getByText('Artist 1 · 100 PLAYS')).toBeVisible();
		await expect.element(hero.getByText('118', { exact: true })).toBeVisible();

		expect(await regionNames()).toEqual([
			'Your year',
			'Top albums',
			'Top artists',
			'Top songs',
			'New to you',
			'By month'
		]);

		const top = page.getByRole('region', { name: 'Top albums' });
		await expect.poll(() => top.getByRole('listitem').elements().length).toBe(9);
		await expect.element(top.getByText('02', { exact: true })).toBeVisible();
		await expect.element(top.getByText('Artist 2 · 95 PLAYS')).toBeVisible();
		await expect.element(top.getByText('55 PLAYS', { exact: true })).toBeVisible();

		await expect
			.element(page.getByRole('region', { name: 'Top artists' }).getByText('No. 2 · 290 PLAYS'))
			.toBeVisible();
		await expect.element(page.getByText('First: Artist 4, 3 FEB')).toBeVisible();
		await expect.element(page.getByText('Album 1 · 41 plays')).toBeVisible();
		await expect.element(page.getByRole('cell', { name: '1 JAN 00:14' })).toBeInTheDocument();
		await expect.element(page.getByRole('link', { name: '2026', exact: true })).toBeVisible();
	});

	it('shows only the count and one card while the year has too few plays', async () => {
		serve({
			...fullYear(),
			state: 'too_few_plays',
			plays: 12,
			top_albums: [],
			top_artists: [],
			top_songs: [],
			busiest_month: null,
			first_play: null,
			busiest_day: null,
			longest_streak: null,
			new_artists: null
		});
		await open('2025');

		await expect.element(page.getByRole('heading', { name: 'Not enough plays yet' })).toBeVisible();
		await expect.element(page.getByText('12', { exact: true })).toBeVisible();
		expect(await regionNames()).toEqual(['Your year']);
		expect(page.getByRole('list').elements()).toHaveLength(0);
		expect(page.getByRole('table').elements()).toHaveLength(0);
	});

	it('leaves out a top list under three and New to you when it is unknown', async () => {
		serve({ ...fullYear(), top_artists: artists.slice(0, 2), new_artists: null });
		await open('2025');

		await expect.element(page.getByRole('region', { name: 'Top albums' })).toBeVisible();
		expect(await regionNames()).toEqual(['Your year', 'Top albums', 'Top songs', 'By month']);
		const hero = page.getByRole('region', { name: 'Your year' });
		await expect.element(hero.getByText('Artists', { exact: true })).toBeVisible();
		await expect.element(hero.getByText('New to you')).not.toBeInTheDocument();
	});
});
