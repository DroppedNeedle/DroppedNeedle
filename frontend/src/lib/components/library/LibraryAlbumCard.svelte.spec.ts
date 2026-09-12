import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LibraryAlbumCard from './LibraryAlbumCard.svelte';
import type { LibraryAlbumSummary } from '$lib/types';

const baseAlbum: LibraryAlbumSummary = {
	id: 'local-album-1',
	title: 'OK Computer',
	artist_name: 'Radiohead',
	artist_id: 'local-artist-1',
	musicbrainz_release_group_id: 'b1392450-e666-3926-a536-22c65f834433',
	musicbrainz_release_id: null,
	musicbrainz_artist_id: null,
	album_identity_state: 'release_group_linked',
	track_count: 12,
	total_duration_seconds: 3200,
	total_size_bytes: 123456,
	format: 'flac',
	year: 1997,
	is_compilation: false,
	cover_available: false,
	date_added: null,
	sort_name: null,
	original_release_date: null,
	contribution_id: null,
	contribution_state: null
};

async function renderComponent(overrides: Partial<LibraryAlbumSummary> = {}) {
	return await render(LibraryAlbumCard, {
		props: { album: { ...baseAlbum, ...overrides } }
	} as Parameters<typeof render<typeof LibraryAlbumCard>>[1]);
}

describe('LibraryAlbumCard.svelte', () => {
	it('shows the album title', async () => {
		await renderComponent();
		await expect.element(page.getByText('OK Computer')).toBeInTheDocument();
	});

	it('shows the album artist', async () => {
		await renderComponent();
		await expect.element(page.getByText(/Radiohead/)).toBeInTheDocument();
	});

	it('shows a FLAC format badge for flac albums', async () => {
		await renderComponent();
		await expect.element(page.getByText('FLAC')).toBeInTheDocument();
	});

	it('shows an MP3 format badge for mp3 albums', async () => {
		await renderComponent({ format: 'mp3' });
		await expect.element(page.getByText('MP3')).toBeInTheDocument();
	});

	it('shows the track count', async () => {
		await renderComponent();
		await expect.element(page.getByText('12 tracks')).toBeInTheDocument();
	});

	it('opens linked albums on their MusicBrainz route', async () => {
		await renderComponent();
		await expect
			.element(page.getByRole('link', { name: 'Open OK Computer' }))
			.toHaveAttribute('href', '/album/b1392450-e666-3926-a536-22c65f834433');
	});

	it('marks local-only albums and keeps their local route', async () => {
		await renderComponent({
			musicbrainz_release_group_id: null,
			album_identity_state: 'local_only'
		});
		await expect
			.element(page.getByRole('link', { name: 'Open OK Computer' }))
			.toHaveAttribute('href', '/album/local-album-1');
		await expect.element(page.getByText('Local-only', { exact: true })).toBeVisible();
	});

	it('does not add an identity badge to linked album cards', async () => {
		await renderComponent();
		await expect.element(page.getByText('Local-only', { exact: true })).not.toBeInTheDocument();
	});
});
