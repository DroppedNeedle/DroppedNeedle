import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import AlbumCard from './AlbumCard.svelte';
import type { Album, EnrichmentSource } from '$lib/types';

// Stub the album-request mutation factory so the card renders without a QueryClientProvider
// (same approach as the TopPicksDeck spec).
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestAlbum: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

const baseAlbum: Album = {
	title: 'OK Computer',
	artist: 'Radiohead',
	year: 1997,
	musicbrainz_id: 'b1392450-e666-3926-a536-22c65f834433',
	in_library: false,
	listen_count: 1200000
};

function renderComponent(
	overrides: Partial<{
		album: Album;
		enrichmentSource: EnrichmentSource;
		onenrichmentrequest: () => void;
	}> = {}
) {
	return render(AlbumCard, {
		props: {
			album: overrides.album ?? baseAlbum,
			enrichmentSource: overrides.enrichmentSource ?? 'none',
			onenrichmentrequest: overrides.onenrichmentrequest
		}
	} as Parameters<typeof render<typeof AlbumCard>>[1]);
}

describe('AlbumCard.svelte', () => {
	it('should display the album title', async () => {
		renderComponent();
		await expect.element(page.getByText('OK Computer')).toBeInTheDocument();
	});

	it('should display artist and year', async () => {
		renderComponent();
		await expect.element(page.getByText(/1997/)).toBeInTheDocument();
		await expect.element(page.getByText(/Radiohead/)).toBeInTheDocument();
	});

	it('should show Last.fm branded badge when source is lastfm', async () => {
		renderComponent({ enrichmentSource: 'lastfm' });

		const badge = page.getByTitle('Last.fm plays');
		await expect.element(badge).toBeInTheDocument();
		await expect.element(page.getByText(/Last\.fm/)).toBeInTheDocument();
	});

	it('should show ListenBrainz branded badge when source is listenbrainz', async () => {
		renderComponent({ enrichmentSource: 'listenbrainz' });

		const badge = page.getByTitle('ListenBrainz plays');
		await expect.element(badge).toBeInTheDocument();
		await expect.element(page.getByText(/LB/)).toBeInTheDocument();
	});

	it('should show generic badge when source is none', async () => {
		renderComponent({ enrichmentSource: 'none' });

		const badge = page.getByTitle('Plays');
		await expect.element(badge).toBeInTheDocument();

		await expect.element(page.getByText(/Last\.fm/)).not.toBeInTheDocument();
		await expect.element(page.getByText(/\bLB\b/)).not.toBeInTheDocument();
	});

	it('should not render listen count badge when listen_count is null', async () => {
		renderComponent({
			album: { ...baseAlbum, listen_count: null },
			enrichmentSource: 'lastfm'
		});

		await expect.element(page.getByTitle('Last.fm plays')).not.toBeInTheDocument();
	});

	it('should render zero listen count as "0"', async () => {
		renderComponent({
			album: { ...baseAlbum, listen_count: 0 },
			enrichmentSource: 'lastfm'
		});

		const badge = page.getByTitle('Last.fm plays');
		await expect.element(badge).toBeInTheDocument();
		await expect.element(page.getByText('Last.fm 0')).toBeInTheDocument();
	});

	it('should display formatted count for large numbers', async () => {
		renderComponent({ enrichmentSource: 'listenbrainz' });

		await expect.element(page.getByText('LB 1.2M')).toBeInTheDocument();
	});

	it('requests optional enrichment on pointer intent', async () => {
		expect.assertions(1);
		const onenrichmentrequest = vi.fn();
		renderComponent({ onenrichmentrequest });

		await page.getByRole('link', { name: 'Open OK Computer' }).hover();

		expect(onenrichmentrequest).toHaveBeenCalledTimes(1);
	});

	it('does not enrich a local-only album with a local id', async () => {
		const onenrichmentrequest = vi.fn();
		renderComponent({
			album: {
				...baseAlbum,
				musicbrainz_id: 'local-album-id',
				local_id: 'local-album-id',
				in_library: true
			},
			onenrichmentrequest
		});

		await page.getByRole('link', { name: 'Open OK Computer' }).hover();

		expect(onenrichmentrequest).not.toHaveBeenCalled();
	});

	it('should use album-specific title for lastfm source', async () => {
		renderComponent({ enrichmentSource: 'lastfm' });

		const badge = page.getByTitle('Last.fm plays');
		await expect.element(badge).toBeInTheDocument();
	});
});
