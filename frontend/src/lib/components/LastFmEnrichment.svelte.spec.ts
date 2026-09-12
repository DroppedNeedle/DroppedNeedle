import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import LastFmEnrichment from './LastFmEnrichment.svelte';
import type { LastFmArtistEnrichment } from '$lib/types';

const fullEnrichment: LastFmArtistEnrichment = {
	bio: 'A legendary rock band formed in the 1960s.',
	summary: null,
	tags: [
		{ name: 'rock', url: 'https://last.fm/tag/rock' },
		{ name: 'classic rock', url: 'https://last.fm/tag/classic+rock' },
		{ name: 'british', url: 'https://last.fm/tag/british' }
	],
	listeners: 2500000,
	playcount: 150000000,
	similar_artists: [],
	url: 'https://www.last.fm/music/TestArtist'
};

async function renderComponent(props: Record<string, unknown> = {}) {
	return await render(LastFmEnrichment, {
		props: { enrichment: fullEnrichment, ...props }
	} as Parameters<typeof render<typeof LastFmEnrichment>>[1]);
}

describe('LastFmEnrichment.svelte', () => {
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

		await expect.element(page.getByText('2.5M listeners')).toBeInTheDocument();
	});

	it('should display formatted play count', async () => {
		await renderComponent();

		await expect.element(page.getByText('150.0M plays')).toBeInTheDocument();
	});

	it('should display bio text', async () => {
		await renderComponent();

		await expect
			.element(page.getByText('A legendary rock band formed in the 1960s.'))
			.toBeInTheDocument();
	});

	it('should render tags as anchor links', async () => {
		await renderComponent();

		const rockLink = page.getByRole('link', { name: 'rock', exact: true });
		await expect.element(rockLink).toBeInTheDocument();
		await expect.element(rockLink).toHaveAttribute('href', '/genre?name=rock');

		const classicRockLink = page.getByRole('link', { name: 'classic rock' });
		await expect.element(classicRockLink).toBeInTheDocument();
		await expect.element(classicRockLink).toHaveAttribute('href', '/genre?name=classic%20rock');
	});

	it('should display View on Last.fm link', async () => {
		await renderComponent();

		const link = page.getByRole('link', { name: /View on Last\.fm/ });
		await expect.element(link).toBeInTheDocument();
		await expect.element(link).toHaveAttribute('href', 'https://www.last.fm/music/TestArtist');
	});

	it('should hide stats section when both counts are zero', async () => {
		await renderComponent({
			enrichment: { ...fullEnrichment, listeners: 0, playcount: 0 }
		});

		await expect.element(page.getByText('listeners')).not.toBeInTheDocument();
	});

	it('should render enrichment with only tags (no bio, no stats)', async () => {
		await renderComponent({
			enrichment: {
				...fullEnrichment,
				bio: null,
				listeners: 0,
				playcount: 0
			}
		});

		await expect.element(page.getByText('Last.fm', { exact: true })).toBeInTheDocument();
		const rockLink = page.getByRole('link', { name: 'rock', exact: true });
		await expect.element(rockLink).toBeInTheDocument();
	});
});
