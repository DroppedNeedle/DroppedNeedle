import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import GapShelfTestHarness from './GapShelfTestHarness.svelte';

const STEELY = [
	{ key: 'royal', year: 1976 },
	{ key: 'cant', year: 1972 },
	{ key: 'pretzel', year: 1974 },
	{ key: 'countdown', year: 1973 }
];

describe('GapShelf', () => {
	it('is one picture named by its sentence, the slot at the missing year', async () => {
		await render(GapShelfTestHarness, {
			items: STEELY,
			year: 1975,
			label: 'Their albums on your shelves, with a space where Katy Lied would go'
		});
		const shelf = page.getByRole('img', {
			name: 'Their albums on your shelves, with a space where Katy Lied would go'
		});
		await expect.element(shelf).toBeVisible();
		await expect
			.element(shelf)
			.toHaveTextContent(
				/^\s*cant\s*1972\s*countdown\s*1973\s*pretzel\s*1974\s*1975\s*royal\s*1976\s*$/
			);
	});

	it('draws a gap with no known year at the end', async () => {
		await render(GapShelfTestHarness, {
			items: STEELY.slice(0, 2),
			year: null,
			label: 'A space at the end'
		});
		const shelf = page.getByRole('img', { name: 'A space at the end' });
		await expect.element(shelf).toHaveTextContent(/^\s*cant\s*1972\s*royal\s*1976\s*$/);
	});
});
