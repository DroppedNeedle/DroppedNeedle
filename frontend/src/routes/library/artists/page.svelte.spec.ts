import { page } from '@vitest/browser/context';
import { beforeEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({ goto: vi.fn() }));

vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));

vi.mock('$lib/components/ArtistImage.svelte', () => {
	const Component = function () {};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryArtistsInfiniteQuery: () => ({
		data: {
			pages: [
				{
					total: 2,
					album_artist_total: 2,
					contributor_total: 1,
					items: [
						{
							id: 'local-linked-artist',
							name: 'Linked Artist',
							musicbrainz_artist_id: 'provider-artist-id',
							artist_identity_state: 'musicbrainz_linked',
							album_count: 2,
							track_count: 20,
							appearance_release_count: 1,
							appearance_track_count: 1,
							library_relationship: 'both',
							date_added: 1,
							row_revision: 1
						},
						{
							id: 'local-only-artist',
							name: 'Local Artist',
							musicbrainz_artist_id: null,
							artist_identity_state: 'local_only',
							album_count: 1,
							track_count: 8,
							appearance_release_count: 0,
							appearance_track_count: 0,
							library_relationship: 'album_artist',
							date_added: 2,
							row_revision: 1
						}
					]
				}
			]
		},
		isError: false,
		isLoading: false,
		hasNextPage: false,
		isFetchingNextPage: false,
		refetch: vi.fn(),
		fetchNextPage: vi.fn()
	})
}));

import ArtistsPage from './+page.svelte';

beforeEach(() => vi.clearAllMocks());

it('uses MusicBrainz routes for linked artists and local routes for local-only artists', async () => {
	await render(ArtistsPage);

	await expect
		.element(page.getByRole('link', { name: 'Open Linked Artist' }))
		.toHaveAttribute('href', '/artist/provider-artist-id');
	await expect
		.element(page.getByRole('link', { name: 'Open Local Artist' }))
		.toHaveAttribute('href', '/artist/local-only-artist');
	await expect.element(page.getByText('Local-only', { exact: true })).toBeVisible();
	await expect
		.element(page.getByText('MusicBrainz linked', { exact: true }))
		.not.toBeInTheDocument();
	await expect.element(page.getByRole('button', { name: /Album artists/ })).toBeVisible();
	await expect.element(page.getByRole('button', { name: /Contributors/ })).toBeVisible();
	await expect.element(page.getByText('2 artists in this view')).toBeVisible();
});

it('keeps the contributor view addressable in the URL', async () => {
	await render(ArtistsPage);

	await page.getByRole('button', { name: /Contributors/ }).click();

	expect(h.goto).toHaveBeenCalledTimes(1);
	const destination = h.goto.mock.calls[0]?.[0];
	expect(destination).toBeInstanceOf(URL);
	expect((destination as URL).searchParams.get('view')).toBe('contributors');
});
