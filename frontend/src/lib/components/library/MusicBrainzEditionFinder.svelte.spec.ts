import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const currentReleaseMbid = '428b6417-8a4d-4a5b-b1a3-8762002167a8';
const h = vi.hoisted(() => ({
	getTitle: (() => '') as () => string,
	getArtist: (() => '') as () => string,
	getOffset: (() => 0) as () => number,
	getEnabled: (() => true) as () => boolean,
	refetch: vi.fn(),
	queryState: {
		data: {
			title_query: 'Local Signals',
			artist_query: 'Signal Artist',
			items: [
				{
					release_mbid: '428b6417-8a4d-4a5b-b1a3-8762002167a8',
					release_group_mbid: 'group-1',
					artist_name: 'Signal Artist',
					title: 'Local Signals',
					date: '2024-02-03',
					country: 'GB',
					status: 'Official',
					packaging: 'Digipak',
					media_formats: ['CD'],
					disc_count: 1,
					track_count: 12,
					label: 'Signal Records',
					catalogue_number: 'SIG-12',
					barcode: '123456',
					disambiguation: 'deluxe booklet',
					musicbrainz_url: 'https://musicbrainz.org/release/428b6417-8a4d-4a5b-b1a3-8762002167a8',
					score: 99,
					belongs_to_current_release_group: true,
					is_current_release: false
				}
			],
			total: 20,
			offset: 0,
			limit: 12
		},
		isLoading: false,
		isFetching: false,
		isError: false
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' } },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));
vi.mock('$lib/queries/library/LibraryEditionQueries.svelte', () => ({
	getReleaseEditionSearchQuery: (
		_getUserId: () => string,
		_getAlbumId: () => string,
		getTitle: () => string,
		getArtist: () => string,
		getOffset: () => number,
		getEnabled: () => boolean
	) => {
		h.getTitle = getTitle;
		h.getArtist = getArtist;
		h.getOffset = getOffset;
		h.getEnabled = getEnabled;
		return { ...h.queryState, refetch: h.refetch };
	}
}));

import MusicBrainzEditionFinder from './MusicBrainzEditionFinder.svelte';

beforeEach(() => {
	vi.clearAllMocks();
	h.queryState.isLoading = false;
	h.queryState.isFetching = false;
	h.queryState.isError = false;
	h.queryState.data.items = [
		{
			...h.queryState.data.items[0],
			release_mbid: currentReleaseMbid,
			is_current_release: false
		}
	];
	h.queryState.data.total = 20;
	h.queryState.data.offset = 0;
	h.refetch.mockResolvedValue({});
});

describe('MusicBrainzEditionFinder', () => {
	it('accepts a canonical MusicBrainz release URL', async () => {
		const oncheck = vi.fn();
		await render(MusicBrainzEditionFinder, {
			props: {
				albumId: 'album-1',
				artistName: 'Signal Artist',
				albumTitle: 'Local Signals',
				oncheck
			}
		} as unknown as Parameters<typeof render>[1]);

		await page.getByText(/Already know the release/).click();
		await page
			.getByRole('textbox', { name: 'MusicBrainz release UUID or URL' })
			.fill(`https://musicbrainz.org/release/${currentReleaseMbid}`);
		await page.getByRole('button', { name: 'Check exact release' }).click();
		expect(oncheck).toHaveBeenCalledWith(currentReleaseMbid);
	});
});
