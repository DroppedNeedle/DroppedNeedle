import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import StarPicker from './StarPicker.svelte';

const face = createRawSnippet(() => ({ render: () => '<span>★</span>' }));

describe('StarPicker', () => {
	it('opens a bar of five stars, hands back the one picked and closes', async () => {
		const onpick = vi.fn();
		await render(StarPicker, { label: 'Rate this track', value: 0, onpick, children: face });

		const button = page.getByRole('button', { name: 'Rate this track' });
		await expect.element(button).toHaveAttribute('title', 'Rate this track');
		await button.click();
		await page.getByRole('button', { name: '4 stars' }).click();

		expect(onpick).toHaveBeenCalledWith(4);
		await expect.element(page.getByRole('button', { name: '4 stars' })).not.toBeInTheDocument();
	});

	it('removes a rating with the bin at the start of the bar', async () => {
		const onpick = vi.fn();
		await render(StarPicker, {
			label: 'Your rating: 3 of 5 stars',
			value: 3,
			clear: 'remove',
			onpick,
			children: face
		});

		await page.getByRole('button', { name: 'Your rating: 3 of 5 stars' }).click();
		await page.getByRole('button', { name: 'Remove rating' }).click();

		expect(onpick).toHaveBeenCalledWith(0);
	});

	it('offers no bin while nothing is rated', async () => {
		await render(StarPicker, {
			label: 'Rate this track',
			value: 0,
			clear: 'remove',
			onpick: vi.fn(),
			children: face
		});

		await page.getByRole('button', { name: 'Rate this track' }).click();
		await expect.element(page.getByRole('button', { name: 'Remove rating' })).toBeDisabled();
	});

	it('switches the floor off with Off', async () => {
		const onpick = vi.fn();
		await render(StarPicker, {
			label: 'Skip tracks with low rating',
			value: 4,
			clear: 'off',
			onpick,
			children: face
		});

		await page.getByRole('button', { name: 'Skip tracks with low rating' }).click();
		await page.getByRole('button', { name: 'Off' }).click();

		expect(onpick).toHaveBeenCalledWith(0);
	});
});
