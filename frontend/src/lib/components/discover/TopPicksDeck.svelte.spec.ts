import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { TopPickItem, TopPicksSection } from '$lib/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

vi.mock('$lib/stores/integration', async () => {
	const { readable } = await import('svelte/store');
	return {
		integrationStore: readable({ download_client: true, youtube: true, youtube_api: true })
	};
});

const section: TopPicksSection = {
	title: 'Top Picks for You',
	source: 'listenbrainz',
	personalizing: false,
	items: [
		{
			album: {
				mbid: 'rg-1',
				name: 'Loveless',
				artist_name: 'My Bloody Valentine',
				artist_mbid: 'artist-1',
				image_url: null,
				release_date: null,
				listen_count: null,
				in_library: false,
				requested: false
			},
			match_pct: 87,
			reasons: ['Because you listen to Slowdive', 'You love shoegaze'],
			seed_artist: 'Slowdive'
		},
		{
			album: {
				mbid: 'rg-2',
				name: 'Souvlaki',
				artist_name: 'Slowdive',
				artist_mbid: 'artist-2',
				image_url: null,
				release_date: null,
				listen_count: null,
				in_library: false,
				requested: false
			},
			match_pct: 72,
			reasons: [],
			seed_artist: null
		}
	]
};

import TopPicksDeck from './TopPicksDeck.svelte';

describe('TopPicksDeck', () => {
	it('renders the featured pick with match percentage and reasons', async () => {
		render(TopPicksDeck, { props: { section } } as Parameters<
			typeof render<typeof TopPicksDeck>
		>[1]);

		await expect.element(page.getByText('87%').first()).toBeVisible();
		await expect.element(page.getByText('Because you listen to Slowdive')).toBeVisible();
		const albumLink = page.getByRole('link', { name: 'Loveless', exact: true });
		await expect.element(albumLink).toHaveAttribute('href', '/album/rg-1');
	});

	it('cycles manually with the arrows', async () => {
		render(TopPicksDeck, { props: { section } } as Parameters<
			typeof render<typeof TopPicksDeck>
		>[1]);

		await page.getByRole('button', { name: 'Next pick' }).click();
		await expect.element(page.getByText('72%', { exact: true }).first()).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Souvlaki', exact: true })).toBeVisible();
	});

	it('promotes a thumbnail to featured on click', async () => {
		render(TopPicksDeck, { props: { section } } as Parameters<
			typeof render<typeof TopPicksDeck>
		>[1]);

		await page.getByRole('button', { name: 'Feature Souvlaki' }).click();
		await expect.element(page.getByRole('link', { name: 'Souvlaki', exact: true })).toBeVisible();
	});

	it('renders nothing for an empty section', async () => {
		render(TopPicksDeck, {
			props: { section: { ...section, items: [] } as TopPicksSection }
		} as Parameters<typeof render<typeof TopPicksDeck>>[1]);

		expect(document.querySelector('section')).toBeNull();
	});

	it('shows the personalising hint only while still warming', async () => {
		render(TopPicksDeck, {
			props: { section: { ...section, personalizing: true } as TopPicksSection }
		} as Parameters<typeof render<typeof TopPicksDeck>>[1]);

		await expect.element(page.getByText('Personalising your picks')).toBeVisible();
	});

	it('dismisses a pick only after the preference is saved', async () => {
		const onignore: (pick: TopPickItem) => Promise<void> = vi.fn(async () => {});
		render(TopPicksDeck, { props: { section, onignore } } as Parameters<
			typeof render<typeof TopPicksDeck>
		>[1]);

		await page.getByRole('button', { name: 'Not for me' }).click();

		expect(onignore).toHaveBeenCalledWith(section.items[0]);
		await expect.element(page.getByRole('link', { name: 'Souvlaki', exact: true })).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Loveless', exact: true }))
			.not.toBeInTheDocument();
	});
});
