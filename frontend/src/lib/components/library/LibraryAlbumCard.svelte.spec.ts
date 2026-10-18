import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryAlbumCard from './LibraryAlbumCard.svelte';
import type { LibraryAlbumSummary } from '$lib/types';

// Keep the image on its first source: the test has no server to answer it.
vi.mock('$lib/utils/lazyImage', () => ({
	lazyImage: () => ({ destroy() {} }),
	resetLazyImage: () => {}
}));

// Local ids are UUID-shaped, so nothing may mistake one for a MusicBrainz id.
const LOCAL_ALBUM_ID = '2abe7c2d-339c-5b5c-8071-7e07a93a6ba6';

const album: LibraryAlbumSummary = {
	id: LOCAL_ALBUM_ID,
	title: 'OK Computer',
	artist_name: 'Radiohead',
	artist_id: 'artist-1',
	musicbrainz_release_group_id: null,
	musicbrainz_release_id: null,
	musicbrainz_artist_id: null,
	album_identity_state: 'local_only',
	track_count: 12,
	total_duration_seconds: 3200,
	total_size_bytes: 1048576,
	format: 'flac',
	year: 1997,
	is_compilation: false,
	release_type: null,
	cover_available: true,
	date_added: 1,
	sort_name: null,
	original_release_date: null,
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

	it('falls back to the release-group cover when a linked album has no art of its own', async () => {
		const releaseGroup = 'b1392450-e666-3926-a536-22c65f834433';
		render(LibraryAlbumCard, {
			props: {
				album: {
					...album,
					cover_available: false,
					musicbrainz_release_group_id: releaseGroup,
					album_identity_state: 'release_group_linked'
				}
			}
		});
		await expect
			.element(page.getByRole('img', { name: 'OK Computer' }))
			.toHaveAttribute('data-src', `/api/v3/covers/release-group/${releaseGroup}?size=250`);
	});
});
