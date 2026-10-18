import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { Album } from '$lib/types';

const { emptyComponent } = vi.hoisted(() => ({
	emptyComponent: () => {
		const C = function () {};
		C.prototype = {};
		return { default: C };
	}
}));

// No download source is set up.
vi.mock('$lib/stores/integration', async () => {
	const { readable } = await import('svelte/store');
	return { integrationStore: readable({ download_client: false, loaded: true }) };
});
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
	requestAlbum: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('./AlbumImage.svelte', emptyComponent);
vi.mock('./AlbumCardOverlay.svelte', emptyComponent);

import AlbumCard from './AlbumCard.svelte';

const album: Album = {
	title: 'OK Computer',
	artist: 'Radiohead',
	year: 1997,
	musicbrainz_id: 'b1392450-e666-3926-a536-22c65f834433',
	in_library: false,
	requested: false
} as Album;

describe('AlbumCard', () => {
	it('offers the request with no download source set up, since the request waits for one', async () => {
		await render(AlbumCard, { album });

		await expect
			.element(page.getByRole('button', { name: 'Request OK Computer' }))
			.toBeInTheDocument();
	});
});
