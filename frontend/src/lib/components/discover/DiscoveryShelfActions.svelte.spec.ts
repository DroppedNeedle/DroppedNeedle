import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { integrationStore } from '$lib/stores/integration';
import { libraryStore } from '$lib/stores/library';
import type { HomeSection } from '$lib/types';
import DiscoveryShelfActions from './DiscoveryShelfActions.svelte';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const section: HomeSection = {
	title: 'Dream Pop Radio',
	type: 'albums',
	source: 'lastfm',
	fallback_message: null,
	connect_service: null,
	items: [
		{
			mbid: 'owned-album',
			name: 'Owned Album',
			artist_name: 'Local Artist',
			artist_mbid: 'local-artist',
			image_url: null,
			release_date: null,
			listen_count: null,
			in_library: true
		},
		{
			mbid: 'new-album',
			name: 'New Album',
			artist_name: 'New Artist',
			artist_mbid: 'new-artist',
			image_url: null,
			release_date: null,
			listen_count: null,
			in_library: false
		},
		{
			mbid: 'requested-album',
			name: 'Requested Album',
			artist_name: 'Queued Artist',
			artist_mbid: 'queued-artist',
			image_url: null,
			release_date: null,
			listen_count: null,
			in_library: false,
			requested: true
		},
		{
			mbid: 'store-requested-album',
			name: 'Store Requested Album',
			artist_name: 'Stored Artist',
			artist_mbid: 'stored-artist',
			image_url: null,
			release_date: null,
			listen_count: null,
			in_library: false
		}
	]
};

describe('DiscoveryShelfActions', () => {
	beforeEach(() => {
		integrationStore.reset();
	});

	it('offers matching play and download actions when acquisition is configured', async () => {
		integrationStore.setStatus({ download_client: true });
		libraryStore.addRequested('store-requested-album');
		await render(DiscoveryShelfActions, {
			section,
			sectionKey: 'radio_sections',
			seed: { seed_type: 'artist', seed_id: 'seed-artist' }
		});

		await expect.element(page.getByRole('button', { name: 'Play all' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Download all' })).toBeVisible();

		await page.getByRole('button', { name: 'Download all' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Download this section' }))
			.toBeVisible();
		await expect.element(page.getByRole('checkbox', { name: /^Requested Album/ })).toBeDisabled();
		await expect
			.element(page.getByRole('checkbox', { name: /^Store Requested Album/ }))
			.toBeDisabled();
		await expect.element(page.getByRole('button', { name: 'Request 1 album' })).toBeVisible();
	});

	it('does not offer downloads without an acquisition source', async () => {
		await render(DiscoveryShelfActions, {
			section,
			sectionKey: 'daily_mixes',
			seed: { seed_type: 'items', items: [] }
		});

		await expect.element(page.getByRole('button', { name: 'Play all' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Download all' }))
			.not.toBeInTheDocument();
	});
});
