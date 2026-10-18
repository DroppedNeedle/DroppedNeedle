import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Card from './Card.svelte';

describe('Card', () => {
	it('is one link named by its title when given an href', async () => {
		const children = createRawSnippet(() => ({ render: () => '<p>12 tracks, 48 minutes</p>' }));
		await render(Card, { href: '/album/7', title: 'Blue Lines', children });

		const links = page.getByRole('link');
		await expect.element(links).toHaveAccessibleName('Blue Lines');
		await expect.element(links).toHaveAttribute('href', '/album/7');
		expect(links.elements()).toHaveLength(1);
	});
});
