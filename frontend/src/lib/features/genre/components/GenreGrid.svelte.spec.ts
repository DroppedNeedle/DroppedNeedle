import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import GenreGrid from './GenreGrid.svelte';

describe('genre grid', () => {
	it('shows nine genres and opens the rest in place, so every genre stays reachable', async () => {
		const genres = Array.from({ length: 20 }, (_, i) => ({ name: `Genre ${i}`, listen_count: i }));
		render(GenreGrid, { title: 'Browse by Genre', genres });

		const links = page.getByRole('link');
		await expect.element(links.nth(8)).toBeInTheDocument();
		expect(links.elements()).toHaveLength(9);

		const more = page.getByRole('button', { name: 'More genres' });
		await expect.element(more).toHaveAttribute('aria-expanded', 'false');
		await more.click();

		await expect.element(page.getByRole('link', { name: /Genre 19/ })).toBeInTheDocument();
		expect(links.elements()).toHaveLength(20);
		await expect
			.element(page.getByRole('button', { name: 'Fewer genres' }))
			.toHaveAttribute('aria-expanded', 'true');
	});
});
