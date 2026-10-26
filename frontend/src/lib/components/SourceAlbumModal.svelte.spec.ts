import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import SourceAlbumModal from './SourceAlbumModal.svelte';

vi.mock('$lib/queries/remotes/remoteApi', () => ({
	remoteApi: { albumTracks: vi.fn(async () => ({ items: [], total: 0, limit: 500, offset: 0 })) }
}));

vi.mock('$lib/queries/playlists/PlaylistV3Mutations.svelte', () => ({
	createPlaylistV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	addPlaylistTracksV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	checkPlaylistTracksV3: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

const ALBUM_MBID = '3c1a7f52-8d0e-4b96-9f21-6a4e2d7b5c18';

describe('SourceAlbumModal', () => {
	it('links the title to the album page and is named by it', async () => {
		render(SourceAlbumModal, {
			open: true,
			sourceType: 'plex',
			album: {
				id: 'plex-1',
				source: 'plex',
				title: 'Night Garden',
				artist_name: 'KIRA',
				track_count: 0,
				release_group_mbid: ALBUM_MBID
			},
			onclose: () => {}
		});

		const dialog = page.getByRole('dialog', { name: 'Night Garden' });
		await expect.element(dialog).toBeVisible();
		await expect
			.element(dialog.getByRole('heading', { name: 'Night Garden' }).getByRole('link'))
			.toHaveAttribute('href', `/album/${ALBUM_MBID}`);
	});
});
