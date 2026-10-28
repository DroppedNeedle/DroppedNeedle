import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import type { ArtistRef } from '$lib/queries/discover/types';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import HomeSection from './HomeSection.svelte';
// The strip's width and scrolling come from Tailwind, so the real styles load.
import '../../app.css';

const mbidOf = (i: number) => `00000000-0000-4000-8000-${String(i).padStart(12, '0')}`;

// Cards are 128 px wide with a 16 px gap on a phone-width page, so the strip
// shows about three. Artist 7 starts roughly 600 px past the strip's right
// edge; Artist 19 is well over 2000 px past it.
const artists: (ArtistRef & { kind: 'artist' })[] = Array.from({ length: 20 }, (_, i) => ({
	kind: 'artist',
	musicbrainz_id: mbidOf(i),
	name: `Artist ${i}`,
	in_library: false
}));

describe('carousel images', () => {
	it('start loading just past the strip edge, and stay lazy far beyond it', async () => {
		// The test server has no covers, so a loaded image errors and polls.
		fakeApi({ 'GET /api/v3/covers/artist/{artist_id}': apiError(404, 'NOT_FOUND') });

		await renderWithQuery(HomeSection, {
			section: {
				title: 'Similar artists',
				type: 'artists',
				items: artists
			}
		});

		// A row not marked eager leaves even its first image to the observer.
		await expect
			.element(page.getByRole('img', { name: 'Artist 0' }))
			.toHaveAttribute('data-src', expect.stringContaining(`/covers/artist/${mbidOf(0)}`));
		// Started loading: the cover address is set, or the test server's missing
		// cover has already swapped the image for its placeholder.
		const artist4 = page.getByRole('img', { name: 'Artist 4' });
		await expect
			.poll(() => {
				const src = artist4.query()?.getAttribute('src');
				return src === undefined || src?.includes(`/covers/artist/${mbidOf(4)}`);
			})
			.toBe(true);
		await expect
			.element(page.getByRole('img', { name: 'Artist 7' }))
			.toHaveAttribute('src', expect.stringMatching(/^data:/));
		await expect
			.element(page.getByRole('img', { name: 'Artist 19' }))
			.toHaveAttribute('src', expect.stringMatching(/^data:/));
	});
});
