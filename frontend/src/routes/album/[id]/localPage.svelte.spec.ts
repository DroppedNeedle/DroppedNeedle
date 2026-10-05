import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { LibraryAlbumDetail, NativeTrackListItem } from '$lib/types';

const h = vi.hoisted(() => ({
	playQueue: vi.fn(),
	addToQueue: vi.fn(),
	playNext: vi.fn(),
	goto: vi.fn(),
	isAdmin: false,
	isTrusted: false,
	editions: undefined as
		| {
				items: Array<{
					release_mbid: string;
					track_count: number;
					title: string | null;
					disambiguation: string | null;
					date: string | null;
					country: string | null;
					packaging: string | null;
					status: string | null;
					is_owned: boolean;
					is_pinned: boolean;
				}>;
				pinned_release_mbid: string | null;
				owned_release_mbid: string | null;
				selected_release_mbid: string | null;
		  }
		| undefined,
	localPin: { pinned_release_mbid: null as string | null },
	setLocalPin: vi.fn(),
	clearLocalPin: vi.fn(),
	toast: vi.fn()
}));

vi.mock('$app/state', () => ({ page: { params: { id: 'local-album-1' } } }));
vi.mock('$app/navigation', () => ({ goto: (...args: unknown[]) => h.goto(...args) }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		get isTrusted() {
			return h.isTrusted;
		},
		user: { id: 'user-1' }
	},
	LAST_USER_ID_KEY: 'test:last-user'
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		playQueue: (...args: unknown[]) => h.playQueue(...args),
		addToQueue: (...args: unknown[]) => h.addToQueue(...args),
		playNext: (...args: unknown[]) => h.playNext(...args)
	}
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => h.toast(...args) }
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
	release_type: 'album',
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
	contribution_state: null,
	display_release_mbid: null,
	pick_basis: null
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
	getAlbumEditionsQuery: () => ({
		get data() {
			return h.editions;
		},
		isLoading: false,
		isError: false
	}),
	getLocalAlbumEditionPinQuery: () => ({
		get data() {
			return h.localPin;
		},
		isLoading: false,
		isError: false
	}),
	setLocalAlbumEditionPin: () => ({ mutateAsync: h.setLocalPin, isPending: false }),
	clearLocalAlbumEditionPin: () => ({ mutateAsync: h.clearLocalPin, isPending: false })
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

const blob = vi.hoisted(() => ({ download: vi.fn() }));
vi.mock('$lib/utils/blobDownload', () => ({ downloadBlob: blob.download }));
// AddToPlaylistModal always mounts (hidden) and creates its mutations at init;
// keep them inert so no QueryClientProvider is needed.
vi.mock('$lib/queries/playlists/PlaylistV3Mutations.svelte', () => ({
	createPlaylistV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	addPlaylistTracksV3: () => ({ mutateAsync: vi.fn(), isPending: false }),
	checkPlaylistTracksV3: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

import LocalAlbumPage from './LocalAlbumPage.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.isAdmin = false;
	h.isTrusted = false;
	h.editions = undefined;
	h.localPin = { pinned_release_mbid: null };
	h.setLocalPin.mockResolvedValue(undefined);
	h.clearLocalPin.mockResolvedValue(undefined);
	album.management_identity_readiness = 'exact_release_required';
	album.identification_status = 'local_metadata';
	album.musicbrainz_release_group_id = null;
	album.musicbrainz_release_id = null;
	album.album_identity_state = 'local_only';
	album.display_release_mbid = null;
	album.pick_basis = null;
	delete album.download_allowed;
});

describe('local-only album page', () => {
	it('shows the edition picker to trusted users', async () => {
		h.isTrusted = true;
		album.musicbrainz_release_group_id = 'rg-1';
		album.album_identity_state = 'release_group_linked';
		h.editions = {
			items: [
				{
					release_mbid: 'rel-1',
					track_count: 10,
					title: 'Album',
					disambiguation: null,
					date: '2020-01-01',
					country: 'XW',
					packaging: null,
					status: 'Official',
					is_owned: false,
					is_pinned: false
				}
			],
			pinned_release_mbid: null,
			owned_release_mbid: null,
			selected_release_mbid: 'rel-1'
		};
		await render(LocalAlbumPage, {
			props: { albumId: album.id }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByRole('button', { name: /Edition:/ })).toBeVisible();
	});

	it('pins an edition through the per-copy local URL', async () => {
		h.isTrusted = true;
		album.musicbrainz_release_group_id = 'rg-1';
		h.editions = {
			items: [
				{
					release_mbid: 'release-11',
					track_count: 11,
					title: 'Local Only Album',
					disambiguation: null,
					date: '2008-08-04',
					country: 'XW',
					packaging: null,
					status: 'Official',
					is_owned: false,
					is_pinned: false
				},
				{
					release_mbid: 'release-20',
					track_count: 20,
					title: 'Local Only Album',
					disambiguation: null,
					date: '2008-08-05',
					country: 'US',
					packaging: null,
					status: 'Official',
					is_owned: false,
					is_pinned: false
				}
			],
			pinned_release_mbid: null,
			owned_release_mbid: null,
			selected_release_mbid: 'release-20'
		};
		await render(LocalAlbumPage, {
			props: { albumId: album.id }
		} as unknown as Parameters<typeof render>[1]);

		await page.getByRole('button', { name: 'Edition: Automatic · 2008 · US · 20 tracks' }).click();
		await page.getByRole('button', { name: '2008 · XW · 11 tracks' }).click();
		await vi.waitFor(() => {
			expect(h.setLocalPin).toHaveBeenCalledWith({
				userId: 'user-1',
				localId: 'local-album-1',
				rgMbid: 'rg-1',
				releaseMbid: 'release-11'
			});
		});
	});
});

describe('local album page track menu', () => {
	beforeEach(() => {
		blob.download.mockResolvedValue(undefined);
	});

	async function renderPage() {
		await render(LocalAlbumPage, {
			props: { albumId: album.id }
		} as unknown as Parameters<typeof render>[1]);
	}

	it('hides the button and omits the menu Download item when restricted', async () => {
		expect.assertions(4);
		album.download_allowed = false;
		await renderPage();

		await expect.element(page.getByText('Unmatched Song')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: /Download album/ }))
			.not.toBeInTheDocument();
		await (await page.getByLabelText('More actions').all())[0].click();
		await expect.element(page.getByRole('menuitem', { name: 'Add to Queue' })).toBeVisible();
		expect(page.getByRole('menuitem', { name: 'Download' }).elements()).toHaveLength(0);
	});
});
