import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const currentReleaseMbid = '428b6417-8a4d-4a5b-b1a3-8762002167a8';
const alternativeReleaseMbid = '718b6417-8a4d-4a5b-b1a3-8762002167a8';
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

vi.mock('$lib/stores/authStore.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/stores/authStore.svelte')>()),
	authStore: { user: { id: 'admin-1' } }
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
	it('keeps artist and release title separate, allows an unknown artist, and pages results', async () => {
		const oncheck = vi.fn();
		await render(MusicBrainzEditionFinder, {
			props: {
				albumId: 'album-1',
				artistName: 'Signal Artist',
				albumTitle: 'Local Signals',
				oncheck
			}
		} as unknown as Parameters<typeof render>[1]);

		const artist = page.getByRole('textbox', { name: /Artist/ });
		const title = page.getByRole('textbox', { name: 'Release title' });
		await expect.element(artist).toHaveValue('Signal Artist');
		await expect.element(title).toHaveValue('Local Signals');
		await artist.fill('Clairo');
		await title.fill('Originals');
		await page.getByRole('button', { name: 'Search', exact: true }).click();
		expect(h.getTitle()).toBe('Originals');
		expect(h.getArtist()).toBe('Clairo');
		await expect.element(page.getByText('Current release group')).toBeVisible();
		await expect.element(page.getByText(/CD · Digipak · 1 disc · 12 tracks/)).toBeVisible();
		const finder = page.getByRole('region', { name: 'Search exact releases' }).element();
		expect(finder.scrollWidth).toBeLessThanOrEqual(finder.clientWidth);
		await page.getByRole('button', { name: /Check this edition/ }).click();
		expect(oncheck).toHaveBeenCalledWith(currentReleaseMbid);
		await page.getByRole('button', { name: /Next/ }).click();
		expect(h.getOffset()).toBe(12);
		await expect
			.element(page.getByRole('link', { name: /Open this search on MusicBrainz/ }))
			.toHaveAttribute(
				'href',
				expect.stringContaining('release%3A%22Originals%22%20AND%20artist%3A%22Clairo%22')
			);

		await artist.fill('');
		await page.getByRole('button', { name: 'Search', exact: true }).click();
		expect(h.getArtist()).toBe('');
	});

	it('shows the attached edition first and reveals only different selectable editions', async () => {
		h.queryState.data.items = [
			{ ...h.queryState.data.items[0], is_current_release: true },
			{
				...h.queryState.data.items[0],
				release_mbid: alternativeReleaseMbid,
				title: 'Local Signals (Deluxe)',
				musicbrainz_url: `https://musicbrainz.org/release/${alternativeReleaseMbid}`,
				is_current_release: false
			}
		];
		const oncheck = vi.fn();
		await render(MusicBrainzEditionFinder, {
			props: {
				albumId: 'album-1',
				artistName: 'Signal Artist',
				albumTitle: 'Local Signals',
				currentReleaseMbid,
				mappedTrackCount: 11,
				totalTrackCount: 12,
				oncheck
			}
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('Currently attached')).toBeVisible();
		await expect.element(page.getByText('11 of 12 indexed files mapped')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: /MusicBrainz/ }))
			.toHaveAttribute('href', `https://musicbrainz.org/release/${currentReleaseMbid}`);
		await expect
			.element(page.getByRole('button', { name: /Check this edition/ }))
			.not.toBeInTheDocument();
		expect(h.getEnabled()).toBe(false);

		await page.getByText('Choose a different edition').click();
		await expect.element(page.getByRole('textbox', { name: 'Release title' })).toHaveFocus();
		expect(h.getEnabled()).toBe(true);
		await expect.element(page.getByText('Local Signals (Deluxe)')).toBeVisible();
		expect(page.getByRole('button', { name: /Check this edition/ }).elements()).toHaveLength(1);
		await page.getByRole('button', { name: /Check this edition/ }).click();
		expect(oncheck).toHaveBeenCalledWith(alternativeReleaseMbid);
	});

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

	it('shows empty and provider-unavailable states independently', async () => {
		h.queryState.data.items = [];
		h.queryState.data.total = 0;
		const empty = await render(MusicBrainzEditionFinder, {
			props: {
				albumId: 'album-1',
				artistName: 'Signal Artist',
				albumTitle: 'Local Signals',
				oncheck: vi.fn()
			}
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByText('No editions found')).toBeVisible();
		await empty.unmount();

		h.queryState.isError = true;
		await render(MusicBrainzEditionFinder, {
			props: {
				albumId: 'album-1',
				artistName: 'Signal Artist',
				albumTitle: 'Local Signals',
				oncheck: vi.fn()
			}
		} as unknown as Parameters<typeof render>[1]);
		await expect.element(page.getByText('MusicBrainz is unavailable')).toBeVisible();
		await page.getByRole('button', { name: 'Retry' }).click();
		expect(h.refetch).toHaveBeenCalledOnce();
	});
});
