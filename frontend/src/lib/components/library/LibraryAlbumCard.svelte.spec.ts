import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryAlbumCard from './LibraryAlbumCard.svelte';
import type { AlbumView } from '$lib/types';

// Keep the image on its first source: the test has no server to answer it.
vi.mock('$lib/utils/lazyImage', () => ({
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
	it("loads the album's own art by its local id", async () => {
		render(LibraryAlbumCard, { props: { album } });
		await expect
			.element(page.getByRole('img', { name: 'OK Computer' }))
			.toHaveAttribute('data-src', `/api/v3/library/albums/${LOCAL_ALBUM_ID}/artwork?size=250`);
	});

	it('keeps year and artist on one line and puts the kind next to the count', async () => {
		render(LibraryAlbumCard, {
			props: { album: { ...album, artist_name: 'T.Raumschmiere', track_count: 4 }, kind: 'Single' }
		});
		const byline = page.getByText('T.Raumschmiere');
		await expect.element(byline).toBeInTheDocument();
		expect(byline.element().closest('p')?.classList.contains('truncate')).toBe(true);
		const count = page.getByText('4 tracks');
		const line = count.element().closest('p');
		expect(line?.textContent?.replace(/\s+/g, '')).toBe('4tracks•Single');
	});

	it('falls back to the release-group cover when a linked album has no art of its own', async () => {
		const releaseGroup = 'b1392450-e666-3926-a536-22c65f834433';
		render(LibraryAlbumCard, {
			props: {
				album: {
					...album,
					cover_available: false,
					release_group_mbid: releaseGroup,
					identity_state: 'linked'
				}
			}
		});
		await expect
			.element(page.getByRole('img', { name: 'OK Computer' }))
			.toHaveAttribute('data-src', `/api/v3/covers/release-group/${releaseGroup}?size=250`);
	});

	it('asks for no cover the server says is missing and shows the placeholder', async () => {
		render(LibraryAlbumCard, {
			props: {
				album: {
					...album,
					cover_available: false,
					art_missing: true,
					release_group_mbid: 'b1392450-e666-3926-a536-22c65f834433',
					identity_state: 'linked'
				}
			}
		});
		await expect.element(page.getByTestId('cover-placeholder')).toBeInTheDocument();
		await expect.element(page.getByRole('img', { name: 'OK Computer' })).not.toBeInTheDocument();
	});
});
