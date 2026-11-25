import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import ScrollRail from './ScrollRail.svelte';

describe('ScrollRail', () => {
	it('greys the letters with no rows and jumps to a letter at its position', async () => {
		await page.viewport(1280, 800);
		const onjump = vi.fn();
		await render(ScrollRail, {
			letters: [
				{ key: '#', position: 0, count: 12 },
				{ key: 'A', position: 12, count: 300 },
				{ key: 'M', position: 31203, count: 900 }
			],
			total: 50000,
			top: 0,
			onjump
		});

		const empty = page.getByRole('button', { name: 'Jump to B' });
		await expect.element(empty).toBeDisabled();
		await empty.click({ force: true });
		expect(onjump).not.toHaveBeenCalled();

		await page.getByRole('button', { name: 'Jump to M' }).click();
		expect(onjump).toHaveBeenCalledWith(31203);
		await expect.element(page.getByText('M · 31,204 of 50,000')).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Numbers and symbols' }))
			.toHaveAttribute('aria-current', 'location');
	});
});
