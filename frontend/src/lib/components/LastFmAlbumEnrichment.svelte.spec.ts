import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LastFmAlbumEnrichment from './LastFmAlbumEnrichment.svelte';
import type { LastFmAlbumEnrichment as LastFmAlbumEnrichmentType } from '$lib/types';

const fullEnrichment: LastFmAlbumEnrichmentType = {
	summary: 'A groundbreaking album released in 1997.',
	tags: [
		{ name: 'alternative', url: 'https://last.fm/tag/alternative' },
		{ name: 'electronic', url: 'https://last.fm/tag/electronic' }
	],
	listeners: 1200000,
	playcount: 45000000,
	url: 'https://www.last.fm/music/TestArtist/TestAlbum'
};

async function renderComponent(props: Record<string, unknown> = {}) {
	return await render(LastFmAlbumEnrichment, {
		props: { enrichment: fullEnrichment, ...props }
	} as Parameters<typeof render<typeof LastFmAlbumEnrichment>>[1]);
}

describe('LastFmAlbumEnrichment.svelte', () => {
	it('should show loading skeleton when loading', async () => {
		await renderComponent({ enrichment: null, loading: true });

		const skeletons = document.querySelectorAll('.skeleton');
		expect(skeletons.length).toBeGreaterThan(0);
	});

	it('should render nothing when not enabled', async () => {
		await renderComponent({ enabled: false });

		await expect.element(page.getByText('Last.fm')).not.toBeInTheDocument();
	});

	it('should render nothing when enrichment is null', async () => {
		await renderComponent({ enrichment: null });

		await expect.element(page.getByText('Last.fm')).not.toBeInTheDocument();
	});

	it('should display Last.fm badge when enrichment is present', async () => {
		await renderComponent();

		await expect.element(page.getByText('Last.fm', { exact: true })).toBeInTheDocument();
	});

	it('should display formatted listener count', async () => {
		await renderComponent();

		await expect.element(page.getByText('1.2M listeners')).toBeInTheDocument();
	});

	it('should display formatted play count', async () => {
		await renderComponent();

		await expect.element(page.getByText('45.0M plays')).toBeInTheDocument();
	});

	it('should display summary text', async () => {
		await renderComponent();

		await expect
			.element(page.getByText('A groundbreaking album released in 1997.'))
			.toBeInTheDocument();
	});

	it('should render tags as anchor links', async () => {
		await renderComponent();

		const altLink = page.getByRole('link', { name: 'alternative' });
		await expect.element(altLink).toBeInTheDocument();
		await expect.element(altLink).toHaveAttribute('href', '/genre?name=alternative');

		const electronicLink = page.getByRole('link', { name: 'electronic' });
		await expect.element(electronicLink).toBeInTheDocument();
		await expect.element(electronicLink).toHaveAttribute('href', '/genre?name=electronic');
	});

	it('should display View on Last.fm link', async () => {
		await renderComponent();

		const link = page.getByRole('link', { name: /View on Last\.fm/ });
		await expect.element(link).toBeInTheDocument();
		await expect
			.element(link)
			.toHaveAttribute('href', 'https://www.last.fm/music/TestArtist/TestAlbum');
	});

	it('should hide stats section when both counts are zero', async () => {
		await renderComponent({
			enrichment: { ...fullEnrichment, listeners: 0, playcount: 0 }
		});

		await expect.element(page.getByText('listeners')).not.toBeInTheDocument();
	});

	it('should render enrichment with only summary (no tags, no stats)', async () => {
		await renderComponent({
			enrichment: {
				...fullEnrichment,
				tags: [],
				listeners: 0,
				playcount: 0
			}
		});

		await expect.element(page.getByText('Last.fm', { exact: true })).toBeInTheDocument();
		await expect
			.element(page.getByText('A groundbreaking album released in 1997.'))
			.toBeInTheDocument();
	});
});
