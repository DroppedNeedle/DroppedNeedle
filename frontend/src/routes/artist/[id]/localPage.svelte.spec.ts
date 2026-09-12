import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryAlbumSummary, LibraryArtistSummary } from '$lib/types';

const h = vi.hoisted(() => ({ goto: vi.fn(), create: vi.fn() }));

vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { isAdmin: false, isTrusted: true, user: { id: 'curator-1' } }
}));

const artist: LibraryArtistSummary = {
	id: 'local-artist-1',
	name: 'Local Artist',
	musicbrainz_artist_id: null,
	artist_identity_state: 'local_only',
	album_count: 3,
	track_count: 30,
	appearance_release_count: 0,
	appearance_track_count: 0,
	library_relationship: 'album_artist',
	date_added: 1,
	row_revision: 1
};

const album = (
	id: string,
	title: string,
	overrides: Partial<LibraryAlbumSummary> = {}
): LibraryAlbumSummary => ({
	id,
	title,
	artist_name: artist.name,
	artist_id: artist.id,
	musicbrainz_release_group_id: null,
	musicbrainz_release_id: null,
	musicbrainz_artist_id: null,
	album_identity_state: 'local_only',
	track_count: 10,
	total_duration_seconds: 1800,
	total_size_bytes: 1024,
	format: 'flac',
	year: 2026,
	is_compilation: false,
	cover_available: false,
	date_added: 1,
	sort_name: null,
	original_release_date: null,
	contribution_id: null,
	contribution_state: null,
	...overrides
});

const albums = [
	album('album-new', 'Unlinked Album'),
	album('album-draft', 'Draft Album', {
		contribution_id: 'contribution-1',
		contribution_state: 'draft'
	}),
	album('album-linked', 'Linked Album', {
		musicbrainz_release_group_id: 'group-1',
		musicbrainz_release_id: 'release-1',
		album_identity_state: 'release_linked'
	}),
	album('album-group-linked', 'Release Group Linked Album', {
		musicbrainz_release_group_id: 'group-2',
		album_identity_state: 'release_group_linked'
	})
];

vi.mock('$lib/queries/library/LibraryQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryQueries.svelte')>()),
	getLibraryArtistDetailQuery: () => ({
		data: artist,
		isLoading: false,
		isError: false,
		refetch: vi.fn()
	}),
	getLibraryArtistAlbumsQuery: () => ({
		data: { items: albums, total: albums.length },
		isLoading: false,
		isError: false
	})
}));

vi.mock('$lib/queries/libraryContributions/LibraryContributionMutations.svelte', () => ({
	createLibraryContributionMutation: () => ({
		isPending: false,
		mutate: (...args: unknown[]) => h.create(...args)
	})
}));

import LocalArtistPage from './LocalArtistPage.svelte';

beforeEach(() => vi.clearAllMocks());

describe('local artist MusicBrainz entry points', () => {
	it('lists unlinked albums and their active contribution state', async () => {
		await render(LocalArtistPage, {
			props: { artistId: artist.id }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('link', { name: 'Find existing MusicBrainz artist' }))
			.toHaveAttribute('href', '#musicbrainz-albums');
		await expect
			.element(page.getByRole('heading', { name: 'Contribute through an album' }))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Unlinked Album', exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('Draft in progress')).toBeVisible();
		await expect.element(page.getByText('Linked Album', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Release Group Linked Album', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Open Release Group Linked Album', exact: true }))
			.toHaveAttribute('href', '/album/group-2');
		await expect
			.element(page.getByRole('link', { name: 'Release Group Linked Album', exact: true }))
			.not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Start with this album' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Resume' })).toBeVisible();
	});

	it('starts a draft or resumes the existing shared contribution', async () => {
		await render(LocalArtistPage, {
			props: { artistId: artist.id }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Start with this album' }).click();
		expect(h.create).toHaveBeenCalledWith('album-new');

		await page.getByRole('button', { name: 'Resume' }).click();
		expect(h.goto).toHaveBeenCalledWith('/library/contributions/contribution-1');
	});
});
