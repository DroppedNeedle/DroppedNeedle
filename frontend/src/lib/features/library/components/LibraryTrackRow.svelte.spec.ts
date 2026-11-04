import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryTrackRow from './LibraryTrackRow.svelte';
import type { TrackView } from '$lib/features/library/queries/types';

const meta: TrackView = {
	id: 'file-1',
	title: 'Airbag',
	album_id: 'album-1',
	album_title: 'OK Computer',
	artist_id: 'artist-1',
	artist_name: 'Radiohead',
	album_artist_id: 'artist-1',
	album_artist_name: 'Radiohead',
	recording_mbid: 'rec-airbag-0001',
	release_group_mbid: null,
	artist_mbid: null,
	album_artist_mbid: null,
	disc_number: 1,
	track_number: 1,
	year: 1997,
	genre: 'Rock',
	format: 'flac',
	bit_rate: 900,
	sample_rate: 44100,
	bit_depth: 16,
	channels: 2,
	duration_seconds: 260,
	file_size_bytes: 1048576,
	date_added: 1,
	cover_available: false,
	art_missing: false,
	favorite: false,
	row_revision: 1
};

async function renderComponent() {
	return await render(LibraryTrackRow, {
		props: { meta }
	} as Parameters<typeof render<typeof LibraryTrackRow>>[1]);
}

describe('LibraryTrackRow.svelte', () => {
	it('shows the stable local track ID without exposing a path', async () => {
		await renderComponent();
		await expect.element(page.getByText(meta.id)).toBeInTheDocument();
	});
});
