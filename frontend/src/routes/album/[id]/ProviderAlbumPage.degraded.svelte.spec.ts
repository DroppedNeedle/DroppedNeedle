import { page } from '@vitest/browser/context';
import { beforeEach, expect, it, vi } from 'vitest';
import { ApiError } from '$lib/api/client';
import { render } from 'vitest-browser-svelte';

// Degraded MusicBrainz-outage fallback: when the provider /basic fetch fails
// but the MB-free library detail query succeeded, the provider pages must
// render the local surface (with an explanatory banner) instead of a
// whole-page error.

const h = vi.hoisted(() => ({
	localView: vi.fn(),
	libraryDetailObserver: vi.fn(),
	refreshAll: vi.fn(),
	stateError: 'Error loading album' as string | null,
	primaryError: null as unknown,
	libraryAlbum: {
		id: 'local-album-id',
		title: 'Outage Album',
		musicbrainz_release_group_id: 'rg-id' as string | null
	} as Record<string, unknown> | null
}));

vi.mock('./albumPageState.svelte', () => ({
	createAlbumPageState: () => ({
		get error() {
			return h.stateError;
		},
		get primaryError() {
			return h.primaryError;
		},
		loadingBasic: false,
		loadingTracks: false,
		tracksError: false,
		loadingDiscovery: false,
		loadingLastfm: false,
		album: null,
		tracksInfo: null,
		showToast: false,
		toastMessage: '',
		toastType: 'success',
		showDeleteModal: false,
		requesting: false,
		refreshing: false,
		headerDownloadTask: null,
		libraryStatus: null,
		albumDownloadTasks: [],
		moreByArtist: null,
		similarAlbums: null,
		trackLinks: [],
		albumLink: null,
		quota: null,
		jellyfinMatch: null,
		localMatch: null,
		navidromeMatch: null,
		plexMatch: null,
		lastfmEnrichment: null,
		renderedTrackSections: [],
		downloadClientConfigured: false,
		headerManagementHeld: [],
		handleDeleted: vi.fn(),
		refreshAll: (...args: unknown[]) => h.refreshAll(...args)
	})
}));

vi.mock('./LocalAlbumPage.svelte', () => {
	const Component = function (_anchor: unknown, props: { albumId: string }) {
		h.localView(props.albumId);
	};
	Component.prototype = {};
	return { default: Component };
});

vi.mock('$lib/queries/library/LibraryQueries.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/library/LibraryQueries.svelte')>()),
	getLibraryAlbumDetailQuery: (...args: unknown[]) => {
		h.libraryDetailObserver(...args);
		return {
			data: h.libraryAlbum,
			isLoading: false,
			error: null
		};
	},
	getLibraryAlbumCopiesQuery: () => ({ data: { items: [] }, isLoading: false }),
	getLibraryAlbumStatusQuery: () => ({ data: null, isLoading: false }),
	getLibraryArtistDetailQuery: () => ({
		data: null,
		isLoading: false,
		error: null
	})
}));

vi.mock('$lib/stores/integration', () => ({
	integrationStore: { subscribe: () => () => {} }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: null, isTrusted: false }
}));

import ProviderAlbumPage from './ProviderAlbumPage.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.primaryError = new ApiError(503, 'Service unavailable');
	h.libraryAlbum = {
		id: 'local-album-id',
		title: 'Outage Album',
		musicbrainz_release_group_id: 'rg-id'
	};
});

it('renders the local surface with a banner when the provider is unavailable', async () => {
	render(ProviderAlbumPage, {
		props: {
			data: { albumId: 'rg-id' },
			localAlbum: h.libraryAlbum
		}
	} as unknown as Parameters<typeof render>[1]);

	await vi.waitFor(() => expect(h.localView).toHaveBeenCalledWith('local-album-id'));
	expect(h.libraryDetailObserver).not.toHaveBeenCalled();
	await expect.element(page.getByText('MusicBrainz is unreachable')).toBeVisible();
});

it('renders a terminal not-found state for a provider 404 even with a local album', async () => {
	h.primaryError = new ApiError(404, 'Album not found');
	render(ProviderAlbumPage, {
		props: {
			data: { albumId: 'missing-album-id' },
			localAlbum: h.libraryAlbum
		}
	} as unknown as Parameters<typeof render>[1]);

	await expect.element(page.getByText('Album not found.')).toBeVisible();
	expect(h.localView).not.toHaveBeenCalled();
	expect(h.libraryDetailObserver).not.toHaveBeenCalled();
});

it('offers an explicit retry when the provider is unavailable without a local album', async () => {
	render(ProviderAlbumPage, {
		props: { data: { albumId: 'unavailable-album-id' } }
	} as unknown as Parameters<typeof render>[1]);

	await expect.element(page.getByText('MusicBrainz is temporarily unavailable.')).toBeVisible();
	await page.getByRole('button', { name: 'Retry' }).click();
	expect(h.refreshAll).toHaveBeenCalledTimes(1);
});

it('keeps other provider errors generic instead of using the local fallback', async () => {
	h.primaryError = new ApiError(400, 'Bad request');
	render(ProviderAlbumPage, {
		props: {
			data: { albumId: 'invalid-album-id' },
			localAlbum: h.libraryAlbum
		}
	} as unknown as Parameters<typeof render>[1]);

	await expect.element(page.getByText('Error loading album')).toBeVisible();
	expect(h.localView).not.toHaveBeenCalled();
	await expect.element(page.getByRole('button', { name: 'Retry' })).not.toBeInTheDocument();
});
