import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryAlbumDetail, NativeTrackListItem } from '$lib/types';

const h = vi.hoisted(() => ({
	playQueue: vi.fn(),
	goto: vi.fn(),
	isAdmin: false
}));

vi.mock('$app/state', () => ({ page: { params: { id: 'local-album-1' } } }));
vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		isTrusted: false,
		user: { id: 'user-1' }
	},
	LAST_USER_ID_KEY: 'test:last-user'
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { playQueue: (...args: unknown[]) => h.playQueue(...args) }
}));

const album: LibraryAlbumDetail = {
	id: 'local-album-1',
	title: 'Local Only Album',
	artist_name: 'Local Artist',
	artist_id: 'local-artist-1',
	musicbrainz_release_group_id: null,
	musicbrainz_release_id: null,
	musicbrainz_artist_id: null,
	album_identity_state: 'local_only',
	track_count: 1,
	total_duration_seconds: 181,
	total_size_bytes: 1024,
	format: 'flac',
	year: 2026,
	is_compilation: false,
	cover_available: false,
	date_added: 1,
	sort_name: null,
	original_release_date: null,
	row_revision: 2,
	input_revision: 'input-2',
	identification_status: 'local_metadata',
	review_id: null,
	review_revision: null,
	management_identity_readiness: 'exact_release_required',
	mapped_track_count: 0,
	management_identity_kind: null,
	custom_manifest_id: null,
	custom_manifest_version: null,
	custom_manifest_track_count: 0,
	custom_manifest_recognized_track_count: 0,
	custom_manifest_stale: false,
	management_excluded: false,
	management_exclusion_revision: null,
	management_excluded_at: null,
	active_edition_conversion: null,
	contribution_id: null,
	contribution_state: null
};

const track: NativeTrackListItem = {
	id: 'local-track-1',
	title: 'Unmatched Song',
	album_id: album.id,
	album_title: album.title,
	artist_id: album.artist_id,
	artist_name: album.artist_name,
	album_artist_id: album.artist_id,
	album_artist_name: album.artist_name,
	musicbrainz_recording_id: null,
	musicbrainz_release_group_id: null,
	musicbrainz_artist_id: null,
	musicbrainz_album_artist_id: null,
	disc_number: 1,
	track_number: 1,
	year: 2026,
	genre: 'Electronic',
	duration_seconds: 181,
	format: 'flac',
	bit_rate: 900000,
	sample_rate: 48000,
	bit_depth: 24,
	channels: 2,
	file_size_bytes: 1024,
	date_added: 1,
	cover_available: false,
	current_tier: null,
	below_cutoff: false
};

vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryAlbumsQuery: () => ({ data: { items: [] } }),
	getLibraryAlbumDetailQuery: () => ({
		data: album,
		isLoading: false,
		isError: false,
		refetch: vi.fn()
	}),
	getLibraryAlbumTracksQuery: () => ({
		data: { items: [track], total: 1, offset: 0, limit: 100 },
		isLoading: false,
		isError: false
	})
}));

vi.mock('$lib/queries/albums/EditionQueries.svelte', () => ({
	getAlbumEditionsQuery: () => ({ data: undefined, isLoading: false, isError: false })
}));

vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryOperationQuery: () => ({ data: undefined, isError: false })
}));

vi.mock('$lib/queries/library/LibraryEditionQueries.svelte', () => ({
	getReleaseEditionSearchQuery: () => ({
		data: {
			title_query: '',
			artist_query: '',
			items: [],
			total: 0,
			offset: 0,
			limit: 12
		},
		isLoading: false,
		isFetching: false,
		isError: false,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/library/LibraryCatalogMutations.svelte', () => {
	const mutation = () => ({
		mutateAsync: vi.fn(),
		isPending: false,
		isError: false,
		reset: vi.fn()
	});
	return {
		reidentifyLibraryAlbum: mutation,
		selectReidentificationCandidate: mutation,
		reenableAlbumManagement: mutation,
		previewAlbumMembership: mutation,
		applyAlbumMembership: mutation
	};
});

vi.mock('$lib/queries/library/LibraryOperationMutations.svelte', () => ({
	controlLibraryOperation: () => ({ mutateAsync: vi.fn() })
}));

vi.mock('$lib/queries/library/EditionConversionQueries.svelte', () => {
	const mutation = () => ({ mutateAsync: vi.fn(), isPending: false, reset: vi.fn() });
	return {
		getEditionConversionQuery: () => ({ data: undefined, refetch: vi.fn() }),
		createEditionConversionPreflight: mutation,
		createEditionConversionPreview: mutation,
		startEditionConversion: mutation,
		retryEditionConversion: mutation,
		recheckEditionConversion: mutation,
		cancelEditionConversion: mutation
	};
});

vi.mock('$lib/queries/libraryContributions/LibraryContributionMutations.svelte', () => ({
	createLibraryContributionMutation: () => ({ isPending: false, mutate: vi.fn() })
}));

import LocalAlbumPage from './LocalAlbumPage.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.isAdmin = false;
	album.management_identity_readiness = 'exact_release_required';
	album.identification_status = 'local_metadata';
});

describe('local-only album page', () => {
	it('plays stable local tracks and presents local identity separately', async () => {
		render(LocalAlbumPage, {
			props: { albumId: album.id }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByRole('heading', { name: 'Local Only Album' })).toBeVisible();
		await expect.element(page.getByText('Local-only', { exact: true })).toBeVisible();
		await expect
			.element(
				page.getByText(
					'This album is in your DroppedNeedle library, but no MusicBrainz release is linked yet.'
				)
			)
			.toBeVisible();
		await expect
			.element(page.getByText('Link a MusicBrainz release group to compare editions.'))
			.toBeVisible();

		await page.getByRole('button', { name: 'Play', exact: true }).click();
		expect(h.playQueue).toHaveBeenCalledWith(
			[
				expect.objectContaining({
					trackSourceId: 'local-track-1',
					sourceType: 'local',
					albumId: 'local-album-1',
					streamUrl: expect.stringContaining('local-track-1')
				})
			],
			0,
			false
		);
	});

	it('warns an administrator when Library Management needs an exact identity', async () => {
		h.isAdmin = true;
		album.management_identity_readiness = 'track_mapping_required';
		render(LocalAlbumPage, {
			props: { albumId: album.id }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('button', { name: 'Re-identify…' }))
			.toHaveClass(/identification-trigger-warning/);
		await expect.element(page.getByText('Exact track map required')).toBeVisible();
	});

	it('does not warn when an exact edition and current track map are ready', async () => {
		h.isAdmin = true;
		album.management_identity_readiness = 'ready';
		album.identification_status = 'identified';
		render(LocalAlbumPage, {
			props: { albumId: album.id }
		} as unknown as Parameters<typeof render>[1]);

		await expect
			.element(page.getByRole('button', { name: 'Re-identify…' }))
			.not.toHaveClass(/identification-trigger-warning/);
	});
});
