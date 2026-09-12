import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { LibraryArtistSummary } from '$lib/types';

import ArtistMergeDialog from './ArtistMergeDialog.svelte';

const artist: LibraryArtistSummary = {
	id: 'artist-1',
	name: 'Primary Artist',
	musicbrainz_artist_id: 'mbid-1',
	artist_identity_state: 'musicbrainz_linked',
	album_count: 3,
	track_count: 20,
	appearance_release_count: 0,
	appearance_track_count: 0,
	library_relationship: 'album_artist',
	date_added: 1,
	row_revision: 4
};

describe('ArtistMergeDialog', () => {
	it('opens the group-oriented identity workflow for the current artist', async () => {
		await render(ArtistMergeDialog, {
			props: { artist }
		} as unknown as Parameters<typeof render>[1]);
		const link = page.getByRole('link', { name: /Open artist identity desk/ });
		await expect.element(link).toBeVisible();
		await expect
			.element(link)
			.toHaveAttribute('href', '/library/management/artists?artist=artist-1&q=Primary%20Artist');
	});
});
