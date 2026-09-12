import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import SearchArtistCard from './SearchArtistCard.svelte';
import type { Artist, EnrichmentSource } from '$lib/types';

const baseArtist: Artist = {
	title: 'Radiohead',
	musicbrainz_id: 'a74b1b7f-71a5-4011-9441-d0b5e4122711',
	in_library: false,
	disambiguation: 'English rock band',
	release_group_count: 9,
	listen_count: 2500000
};

async function renderComponent(
	overrides: Partial<{
		artist: Artist;
		enrichmentSource: EnrichmentSource;
		onenrichmentrequest: () => void;
	}> = {}
) {
	return await render(SearchArtistCard, {
		props: {
			artist: overrides.artist ?? baseArtist,
			enrichmentSource: overrides.enrichmentSource ?? 'none',
			onenrichmentrequest: overrides.onenrichmentrequest
		}
	} as Parameters<typeof render<typeof SearchArtistCard>>[1]);
}

describe('SearchArtistCard.svelte', () => {
	it('should display the artist name', async () => {
		await renderComponent();
		await expect.element(page.getByText('Radiohead')).toBeInTheDocument();
	});

	it('should display release count badge', async () => {
		await renderComponent();
		await expect.element(page.getByText('9 releases')).toBeInTheDocument();
	});

	it('should show Last.fm branded badge when source is lastfm', async () => {
		await renderComponent({ enrichmentSource: 'lastfm' });

		const badge = page.getByTitle('Last.fm listeners');
		await expect.element(badge).toBeInTheDocument();
		await expect.element(page.getByText(/Last\.fm/)).toBeInTheDocument();
	});

	it('should show ListenBrainz branded badge when source is listenbrainz', async () => {
		await renderComponent({ enrichmentSource: 'listenbrainz' });

		const badge = page.getByTitle('ListenBrainz plays');
		await expect.element(badge).toBeInTheDocument();
		await expect.element(page.getByText(/LB/)).toBeInTheDocument();
	});

	it('should show generic badge when source is none', async () => {
		await renderComponent({ enrichmentSource: 'none' });

		const badge = page.getByTitle('Plays');
		await expect.element(badge).toBeInTheDocument();

		await expect.element(page.getByText(/Last\.fm/)).not.toBeInTheDocument();
		await expect.element(page.getByText(/\bLB\b/)).not.toBeInTheDocument();
	});

	it('should not render listen count badge when listen_count is null', async () => {
		await renderComponent({
			artist: { ...baseArtist, listen_count: null },
			enrichmentSource: 'lastfm'
		});

		await expect.element(page.getByTitle('Last.fm listeners')).not.toBeInTheDocument();
	});

	it('should render zero listen count as "0"', async () => {
		await renderComponent({
			artist: { ...baseArtist, listen_count: 0 },
			enrichmentSource: 'lastfm'
		});

		const badge = page.getByTitle('Last.fm listeners');
		await expect.element(badge).toBeInTheDocument();
		await expect.element(page.getByText('Last.fm 0')).toBeInTheDocument();
	});

	it('should display formatted count for large numbers', async () => {
		await renderComponent({ enrichmentSource: 'lastfm' });

		await expect.element(page.getByText('Last.fm 2.5M')).toBeInTheDocument();
	});

	it('should display disambiguation when present', async () => {
		await renderComponent();
		await expect.element(page.getByText('English rock band')).toBeInTheDocument();
	});

	it('requests optional enrichment on keyboard focus', async () => {
		expect.assertions(2);
		const onenrichmentrequest = vi.fn();
		await renderComponent({ onenrichmentrequest });
		const artistName = page.getByText('Radiohead');
		await expect.element(artistName).toBeInTheDocument();
		onenrichmentrequest.mockClear();

		artistName.element().closest('a')?.focus();

		expect(onenrichmentrequest).toHaveBeenCalledTimes(1);
	});

	it('does not enrich a local-only artist with a local id', async () => {
		const onenrichmentrequest = vi.fn();
		await renderComponent({
			artist: {
				...baseArtist,
				musicbrainz_id: 'local-artist-id',
				local_id: 'local-artist-id',
				in_library: true
			},
			onenrichmentrequest
		});

		await page.getByText('Radiohead').hover();

		expect(onenrichmentrequest).not.toHaveBeenCalled();
	});

	it('should singular release for count of 1', async () => {
		await renderComponent({
			artist: { ...baseArtist, release_group_count: 1 }
		});
		await expect.element(page.getByText('1 release')).toBeInTheDocument();
	});
});
