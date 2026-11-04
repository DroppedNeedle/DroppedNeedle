import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi, onTestFinished } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
// Keep the image on its first source: the test has no server to answer it.
vi.mock('$lib/media/lazyImage', () => ({
	lazyImage: () => ({ destroy() {} }),
	resetLazyImage: () => {}
}));

import SearchPage from './+page.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import { toAuthUser } from '$lib/features/auth/queries/types';

type SearchResponse = RouteData<'GET /api/v3/search'>;

const empty: SearchResponse = {
	artists: [],
	albums: [],
	tracks: [],
	top_artist: null,
	top_album: null,
	top_track: null,
	artist_status: 'ok',
	album_status: 'ok',
	track_status: 'ok'
};

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

afterEach(() => {
	vi.restoreAllMocks();
});

// Two library albums with no MusicBrainz id: one with art of its own, one the
// server says has none.
const withAlbums: SearchResponse = {
	...empty,
	albums: [
		{
			kind: 'album',
			id: 'album-art',
			title: 'Kitchen Tapes',
			artist: 'Someone',
			year: 2020,
			musicbrainz_id: null,
			in_library: true,
			requested: false,
			score: 70,
			disambiguation: null,
			type_info: null,
			art_missing: false,
			track: null
		},
		{
			kind: 'album',
			id: 'album-bare',
			title: 'Home Demos',
			artist: 'Someone',
			year: 2021,
			musicbrainz_id: null,
			in_library: true,
			requested: false,
			score: 70,
			disambiguation: null,
			type_info: null,
			art_missing: true,
			track: null
		}
	]
};

describe('search page album covers', () => {
	it('asks for no cover the server says is missing and shows the placeholder', async () => {
		fakeApi({ 'GET /api/v3/search': withAlbums });
		// The cover has no file here, so its image can fail and leave the page
		// before a locator sees it: note each image as it arrives.
		const asked: string[] = [];
		const images = new MutationObserver((records) => {
			for (const node of records.flatMap((record) => [...record.addedNodes])) {
				if (!(node instanceof Element)) continue;
				const found = node instanceof HTMLImageElement ? [node] : node.getElementsByTagName('img');
				for (const image of found) asked.push(image.src);
			}
		});
		images.observe(document.body, { childList: true, subtree: true });
		onTestFinished(() => images.disconnect());
		await renderWithQuery(SearchPage, { data: { query: 'someone' } });

		const bare = page.getByRole('link', { name: 'Open Home Demos' });
		await expect.element(bare.getByTestId('cover-placeholder')).toBeInTheDocument();
		await expect.element(page.getByRole('img', { name: 'Home Demos' })).not.toBeInTheDocument();
		await expect
			.poll(() => asked)
			.toContainEqual(expect.stringContaining('/library/albums/album-art/artwork'));
		expect(asked).not.toContainEqual(expect.stringContaining('/library/albums/album-bare/'));
	});
});
