import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type {
	LibraryAlbumSummary,
	LibraryArtistAppearancesResponse,
	NativeTrackListItem
} from '$lib/types';

const h = vi.hoisted(() => ({
	playQueue: vi.fn(),
	refetch: vi.fn(),
	fetchNextPage: vi.fn(),
	data: null as { pages: LibraryArtistAppearancesResponse[]; pageParams: number[] } | null,
	isLoading: false,
	isError: false,
	hasNextPage: false,
	isFetchingNextPage: false
}));

vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		playQueue: (...args: unknown[]) => h.playQueue(...args)
	}
}));

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryArtistAppearancesQuery: () => ({
		get data() {
			return h.data;
		},
		get isLoading() {
			return h.isLoading;
		},
		get isError() {
			return h.isError;
		},
		get hasNextPage() {
			return h.hasNextPage;
		},
		get isFetchingNextPage() {
			return h.isFetchingNextPage;
		},
		refetch: (...args: unknown[]) => h.refetch(...args),
		fetchNextPage: (...args: unknown[]) => h.fetchNextPage(...args)
	})
}));

const album: LibraryAlbumSummary = {
	id: 'album-local-1',
	title: 'Night Drive',
	artist_name: 'The Headliners',
	artist_id: 'artist-headliners',
	musicbrainz_release_group_id: 'release-group-1',
	musicbrainz_release_id: 'release-1',
	musicbrainz_artist_id: 'mbid-headliners',
	album_identity_state: 'release_linked',
	track_count: 10,
	total_duration_seconds: 2400,
	total_size_bytes: 1000,
	format: 'flac',
	year: 2024,
	is_compilation: false,
	cover_available: false,
	date_added: 1,
	sort_name: null,
	original_release_date: '2024-01-01',
	contribution_id: null,
	contribution_state: null
};

function track(id: string, title: string, trackNumber: number): NativeTrackListItem {
	return {
		id,
		title,
		album_id: album.id,
		album_title: album.title,
		artist_id: 'artist-guest',
		artist_name: 'Guest Artist',
		album_artist_id: album.artist_id,
		album_artist_name: album.artist_name,
		musicbrainz_recording_id: `recording-${id}`,
		musicbrainz_release_group_id: album.musicbrainz_release_group_id,
		musicbrainz_artist_id: 'mbid-guest',
		musicbrainz_album_artist_id: album.musicbrainz_artist_id,
		disc_number: 1,
		track_number: trackNumber,
		year: album.year,
		genre: 'Alternative',
		duration_seconds: 200,
		format: 'flac',
		bit_rate: 900000,
		sample_rate: 44100,
		bit_depth: 16,
		channels: 2,
		file_size_bytes: 500,
		date_added: 1,
		cover_available: false,
		current_tier: null,
		below_cutoff: false
	};
}

const appearanceTracks = [track('track-2', 'Second Voice', 2), track('track-7', 'Afterglow', 7)];

import ArtistAppearancesSection from './ArtistAppearancesSection.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.isLoading = false;
	h.isError = false;
	h.hasNextPage = false;
	h.isFetchingNextPage = false;
	h.data = {
		pages: [
			{
				items: [{ album, tracks: appearanceTracks }],
				total: 1,
				total_tracks: 2,
				offset: 0,
				limit: 20
			}
		],
		pageParams: [0]
	};
});

describe('ArtistAppearancesSection', () => {
	it('groups exact local appearances under their owned release', async () => {
		await render(ArtistAppearancesSection, {
			props: { artistId: 'mbid-guest' }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('heading', { name: 'Appears in your library' }))
			.toBeVisible();
		await expect.element(page.getByText('1 release', { exact: true })).toBeVisible();
		await expect.element(page.getByText('2 tracks', { exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Night Drive', exact: true }))
			.toHaveAttribute('href', '/album/release-group-1');
		await expect.element(page.getByText('Second Voice', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Afterglow', { exact: true })).toBeVisible();
	});

	it('uses the track controls as the only appearance playback actions', async () => {
		await render(ArtistAppearancesSection, {
			props: { artistId: 'mbid-guest' }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('button', { name: 'Play appearances' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Play Second Voice' }).click();

		expect(h.playQueue).toHaveBeenCalledTimes(1);
		expect(h.playQueue).toHaveBeenCalledWith(
			expect.arrayContaining([
				expect.objectContaining({ trackSourceId: 'track-2' }),
				expect.objectContaining({ trackSourceId: 'track-7' })
			]),
			0,
			false
		);
	});

	it('renders no empty contributor section when there are no appearances', async () => {
		h.data = {
			pages: [{ items: [], total: 0, total_tracks: 0, offset: 0, limit: 20 }],
			pageParams: [0]
		};
		await render(ArtistAppearancesSection, {
			props: { artistId: 'mbid-guest' }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('heading', { name: 'Appears in your library' }))
			.not.toBeInTheDocument();
	});

	it('offers a retry without confusing an API failure for an empty result', async () => {
		h.data = null;
		h.isError = true;
		await render(ArtistAppearancesSection, {
			props: { artistId: 'mbid-guest' }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByText("Couldn't load this artist's local track appearances."))
			.toBeVisible();
		await page.getByRole('button', { name: 'Retry' }).click();
		expect(h.refetch).toHaveBeenCalledTimes(1);
	});

	it('makes a partial release list explicit and can load its next page', async () => {
		h.hasNextPage = true;
		h.data = {
			pages: [
				{
					items: [{ album, tracks: appearanceTracks }],
					total: 23,
					total_tracks: 26,
					offset: 0,
					limit: 20
				}
			],
			pageParams: [0]
		};
		await render(ArtistAppearancesSection, {
			props: { artistId: 'mbid-guest' }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('Showing 1 of 23 local releases')).toBeVisible();
		await page.getByRole('button', { name: 'Load more appearances' }).click();
		expect(h.fetchNextPage).toHaveBeenCalledTimes(1);
	});
});
