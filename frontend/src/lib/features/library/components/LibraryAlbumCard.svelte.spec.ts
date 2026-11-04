import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryAlbumCard from './LibraryAlbumCard.svelte';
import type { AlbumView } from '$lib/types';

// Keep the image on its first source: the test has no server to answer it.
vi.mock('$lib/media/lazyImage', () => ({
	lazyImage: () => ({ destroy() {} }),
	resetLazyImage: () => {}
}));

// Local ids are UUID-shaped, so nothing may mistake one for a MusicBrainz id.
const LOCAL_ALBUM_ID = '2abe7c2d-339c-5b5c-8071-7e07a93a6ba6';

const album: AlbumView = {
	id: LOCAL_ALBUM_ID,
	title: 'OK Computer',
	artist_name: 'Radiohead',
	artist_id: 'artist-1',
	release_group_mbid: null,
	release_mbid: null,
	artist_mbid: null,
	identity_state: 'local_only',
	track_count: 12,
	total_duration_seconds: 3200,
	total_size_bytes: 1048576,
	format: 'flac',
	year: 1997,
	is_compilation: false,
	cover_available: true,
	art_missing: false,
	favorite: false,
	date_added: 1,
	contribution_id: null,
	contribution_state: null
};

describe('LibraryAlbumCard.svelte', () => {
	it('loads its own art, else the release-group cover, and asks for none the server says is missing', async () => {
		const releaseGroup = 'b1392450-e666-3926-a536-22c65f834433';
		const linked = {
			...album,
			cover_available: false,
			release_group_mbid: releaseGroup,
			identity_state: 'linked'
		} as const;
		const cases: [AlbumView, string | null][] = [
			[album, `/api/v3/library/albums/${LOCAL_ALBUM_ID}/artwork?size=250`],
			[linked, `/api/v3/covers/release-group/${releaseGroup}?size=250`],
			[{ ...linked, art_missing: true }, null]
		];
		for (const [shown, source] of cases) {
			const view = await render(LibraryAlbumCard, { props: { album: shown } });
			const image = page.getByRole('img', { name: 'OK Computer' });
			if (source) {
				await expect.element(image).toHaveAttribute('data-src', source);
			} else {
				await expect.element(page.getByTestId('cover-placeholder')).toBeInTheDocument();
				await expect.element(image).not.toBeInTheDocument();
			}
			view.unmount();
		}
	});
});
