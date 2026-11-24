import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import SegmentedChoice from './SegmentedChoice.svelte';

describe('SegmentedChoice', () => {
	it('selects by click and by arrow key, telling once each time', async () => {
		const onchange = vi.fn();
		await render(SegmentedChoice, {
			label: 'Request',
			options: [
				{ value: 'songs', label: 'Songs' },
				{ value: 'albums', label: 'Whole albums' }
			],
			value: 'songs',
			onchange
		});

		const group = page.getByRole('radiogroup', { name: 'Request' });
		await expect.element(group).toBeVisible();
		await page.getByRole('radio', { name: 'Whole albums' }).click();
		await expect.element(page.getByRole('radio', { name: 'Whole albums' })).toBeChecked();
		expect(onchange).toHaveBeenCalledTimes(1);
		expect(onchange).toHaveBeenLastCalledWith('albums');

		await userEvent.keyboard('{ArrowLeft}');
		await expect.element(page.getByRole('radio', { name: 'Songs' })).toBeChecked();
		expect(onchange).toHaveBeenCalledTimes(2);
		expect(onchange).toHaveBeenLastCalledWith('songs');
	});
});
