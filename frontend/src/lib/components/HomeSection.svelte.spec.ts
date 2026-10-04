import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// Album cards read the download-access flag; stub it so the section renders
// without a QueryClientProvider.
vi.mock('$lib/queries/local/LocalQueries.svelte', () => ({
	getDownloadAccessQuery: () => ({ data: { allowed: true } })
}));

import HomeSection from './HomeSection.svelte';

describe('HomeSection.svelte', () => {
	it('routes a local-only artist through its stable DroppedNeedle identity', async () => {
		await render(HomeSection, {
			props: {
				section: {
					title: 'Your Artists',
					type: 'artists',
					items: [
						{
							name: 'Local Artist',
							mbid: null,
							local_id: 'local-artist-1',
							in_library: true
						}
					]
				}
			}
		} as unknown as Parameters<typeof render<typeof HomeSection>>[1]);

		await expect
			.element(page.getByRole('link', { name: /Local Artist/ }))
			.toHaveAttribute('href', '/artist/local-artist-1');
	});

	it('prefers the familiar provider route when both identities exist', async () => {
		await render(HomeSection, {
			props: {
				section: {
					title: 'Your Albums',
					type: 'albums',
					items: [
						{
							name: 'Identified Album',
							artist_name: 'Identified Artist',
							mbid: 'provider-album-1',
							local_id: 'local-album-1',
							in_library: true
						}
					]
				}
			}
		} as unknown as Parameters<typeof render<typeof HomeSection>>[1]);

		await expect
			.element(page.getByRole('link', { name: /Identified Album/ }))
			.toHaveAttribute('href', '/album/provider-album-1');
	});

	it('links a local-only album without nesting a search action inside the card', async () => {
		await render(HomeSection, {
			props: {
				section: {
					title: 'Your Albums',
					type: 'albums',
					items: [
						{
							name: 'Local Only Album',
							artist_name: 'Local Artist',
							mbid: null,
							local_id: 'local-only-album-1',
							in_library: true
						}
					]
				}
			}
		} as unknown as Parameters<typeof render<typeof HomeSection>>[1]);

		const link = page.getByRole('link', { name: /Local Only Album/ });
		await expect.element(link).toHaveAttribute('href', '/album/local-only-album-1');
		await expect.element(link.getByRole('button')).not.toBeInTheDocument();
	});
});
