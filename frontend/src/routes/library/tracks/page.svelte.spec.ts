import { page, userEvent } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import TracksPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/queries/auth/types';

type TrackPage = RouteData<'GET /api/v3/library/tracks'>;

function track(id: string, title: string, artist: string): TrackPage['items'][number] {
	return {
		id,
		title,
		album_id: `album-${id}`,
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
		favorite: false
	};
}

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

describe('library tracks page', () => {
	it('sorts by artist and loads more after the last page by its cursor', async () => {
		const api = fakeApi({
			'GET /api/v3/library/tracks': ({ query }): TrackPage =>
				query.get('after') === 'page-2'
					? { items: [track('t2', 'Second Song', 'Boreal')], total: 2, next: null }
					: { items: [track('t1', 'First Song', 'Aurora')], total: 2, next: 'page-2' }
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
});
