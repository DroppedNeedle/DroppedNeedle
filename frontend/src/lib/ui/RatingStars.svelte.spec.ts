import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import RatingStars from './RatingStars.svelte';

describe('RatingStars', () => {
	it('rates an unrated item with the star clicked', async () => {
		const onchange = vi.fn();
		await render(RatingStars, { value: 0, label: 'Teardrop', onchange });

		await expect.element(page.getByRole('group')).toHaveAccessibleName('Teardrop: not rated');
		await page.getByRole('button', { name: '3 stars' }).click();
		expect(onchange).toHaveBeenCalledWith(3);
	});

	it('clears the rating when the star given is clicked again', async () => {
		const onchange = vi.fn();
		await render(RatingStars, { value: 3, label: 'Teardrop', onchange });

		await expect.element(page.getByRole('group')).toHaveAccessibleName('Teardrop: 3 of 5 stars');
		const given = page.getByRole('button', { name: '3 stars' });
		await expect.element(given).toHaveAttribute('aria-pressed', 'true');
		await given.click();
		expect(onchange).toHaveBeenCalledWith(0);
	});
});
