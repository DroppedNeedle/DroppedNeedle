import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import GenreGrid from './GenreGrid.svelte';

describe('GenreGrid.svelte', () => {
	it('uses the genre as the only accessible link name and preserves keyboard focus', async () => {
		await render(GenreGrid, {
			props: {
				title: 'Browse Genres',
				genres: [{ name: 'Electronic', listen_count: 1200 }],
				genreArtwork: {
					Electronic: { kind: 'gradient', albums: [], version: 'v2:0:test' }
				}
			}
		} as unknown as Parameters<typeof render<typeof GenreGrid>>[1]);
		const link = page.getByRole('link', { name: /Electronic/ });

		await expect.element(link).toHaveAttribute('href', '/genre?name=Electronic');
		(link.element() as HTMLElement).focus();
		await expect.element(link).toHaveFocus();
		await expect.element(link).toHaveClass(/genre-tile/);
	});
});
