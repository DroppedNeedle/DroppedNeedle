import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ArtistInfo } from '$lib/types';

vi.mock('$lib/queries/following/FollowQueries.svelte', () => ({
	getFollowStatusQuery: () => ({
		data: { followed: false, auto_download: false, auto_download_state: 'none' },
		isPending: false
	})
}));

vi.mock('$lib/queries/following/FollowMutations.svelte', () => ({
	createSetFollowMutation: () => ({ isPending: false, mutate: vi.fn() }),
	createSetAutoDownloadMutation: () => ({ isPending: false, mutate: vi.fn() })
}));

const artist: ArtistInfo = {
	name: 'Guest Artist',
	musicbrainz_id: 'artist-mbid',
	disambiguation: null,
	type: 'Person',
	country: null,
	life_span: null,
	fanart_url: null,
	banner_url: null,
	thumb_url: null,
	fanart_url_2: null,
	fanart_url_3: null,
	fanart_url_4: null,
	wide_thumb_url: null,
	logo_url: null,
	clearart_url: null,
	cutout_url: null,
	tags: [],
	aliases: [],
	external_links: [],
	in_library: false,
	appears_in_library: true
};

import ArtistHero from './ArtistHero.svelte';

beforeEach(() => vi.clearAllMocks());

describe('ArtistHero library relationship', () => {
	it('labels a track-only contributor as appearing in the library', async () => {
		await render(ArtistHero, {
			props: { artist }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('Appears in library', { exact: true })).toBeVisible();
		await expect.element(page.getByText('In Library', { exact: true })).not.toBeInTheDocument();
	});

	it('prioritizes album ownership when an artist is both owned and a contributor', async () => {
		await render(ArtistHero, {
			props: { artist: { ...artist, in_library: true, appears_in_library: true } }
		} as unknown as Parameters<typeof render>[1]);

		await expect.element(page.getByText('In Library', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Appears in library', { exact: true }))
			.not.toBeInTheDocument();
	});
});
