import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type {
	AlbumCardV3,
	LocalSearchResultsV3,
	SuggestionTrackV3
} from '$lib/queries/local/LocalV3Queries.svelte';
import SearchCard from './SearchCard.svelte';

const album: AlbumCardV3 = {
	id: 'al1',
	title: 'First Light',
	artist_name: 'Aurora',
	artist_mbid: 'm1',
	release_group_mbid: 'rg1',
	year: 1994,
	track_count: 2,
	total_size_bytes: 8000000,
	primary_format: 'flac',
	cover_available: true,
	date_added: 1000
};

const track: SuggestionTrackV3 = {
	track_id: 't1',
	title: 'Opener',
	album_title: 'First Light',
	artist_name: 'Aurora',
	album_id: 'al1',
	cover_available: true,
	format: 'flac',
	year: 1994,
	duration_seconds: 200,
	reason: 'match'
};

const results: LocalSearchResultsV3 = { albums: [album], tracks: [track] };

// The data getter re-reads the debounced term so rows appear once the
// component's debounce fires, mirroring the real query's reactivity.
vi.mock('$lib/queries/local/LocalV3Queries.svelte', () => ({
	getLocalSearchV3Query: (getTerm: () => string) => ({
		get data() {
			return getTerm().trim().length >= 2 ? results : undefined;
		},
		isFetching: false
	})
}));

describe('SearchCard.svelte', () => {
	it('renders adapted album and track rows with v3 covers', async () => {
		const onPlayTrack = vi.fn();
		const onPlayAlbum = vi.fn();
		await render(SearchCard, {
			props: {
				reducedMotion: true,
				onPlayTrack,
				onQueueTrack: vi.fn(),
				onPlayAlbum,
				onQueueAlbum: vi.fn()
			}
		} as Parameters<typeof render<typeof SearchCard>>[1]);

		await page.getByLabelText('Search your library for albums and songs').fill('li');
		await expect.element(page.getByText('First Light', { exact: true })).toBeInTheDocument();
		await expect.element(page.getByText('Opener')).toBeInTheDocument();
		await expect
			.element(page.getByRole('img').first())
			.toHaveAttribute('src', '/api/v3/covers/release-group/rg1?size=250');

		await (await page.getByRole('button', { name: 'Play album', exact: true }).all())[0].click();
		expect(onPlayAlbum).toHaveBeenCalledWith(expect.objectContaining({ name: 'First Light' }));
		await (await page.getByRole('button', { name: 'Play now', exact: true }).all())[0].click();
		expect(onPlayTrack).toHaveBeenCalledWith(expect.objectContaining({ title: 'Opener' }));
	});
});
