import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Bin, { type BinRow } from './Bin.svelte';

const row = (n: number, title: string, artist: string): BinRow => ({
	key: `rg-${n}`,
	number: `0${n}`,
	title,
	artist
});

const cover = createRawSnippet<[BinRow]>(() => ({ render: () => '<span></span>' }));
const state = createRawSnippet<[BinRow, boolean]>((_row, isCurrent) => ({
	render: () => `<span>${isCurrent() ? 'On the counter' : ''}</span>`
}));

describe('Bin', () => {
	it('files its rows in lists named by their tabs, with the one on the counter marked current', async () => {
		const onselect = vi.fn();
		await render(Bin, {
			title: "This week's eight",
			tally: '3 of 8 answered',
			label: 'Week 47 - holds still until Fri 27 Nov',
			current: 'rg-2',
			onselect,
			cover,
			state,
			groups: [
				{
					name: 'Around Poppy',
					share: '35%',
					shareSpoken: '35% of your listening',
					rows: [row(1, 'SAWAYAMA', 'Rina Sawayama'), row(2, 'Cape God', 'Allie X')]
				},
				{ name: 'Around Yes', rows: [row(3, 'Rumours', 'Fleetwood Mac')] }
			]
		});

		await expect.element(page.getByRole('heading', { name: "This week's eight" })).toBeVisible();
		await expect.element(page.getByText('Week 47 - holds still until Fri 27 Nov')).toBeVisible();

		// Each tab names its group's list.
		const poppy = page.getByRole('list', { name: 'Around Poppy, 35% of your listening' });
		await expect.element(poppy).toBeVisible();
		expect(poppy.getByRole('listitem').all()).toHaveLength(2);
		const yes = page.getByRole('list', { name: 'Around Yes' });
		expect(yes.getByRole('listitem').all()).toHaveLength(1);

		// The row on the counter, and only that one, is current.
		const current = page.getByRole('button', { name: /Cape God/ });
		await expect.element(current).toHaveAttribute('aria-current', 'true');
		await expect.element(current).toHaveTextContent('On the counter');
		await expect
			.element(page.getByRole('button', { name: /SAWAYAMA/ }))
			.not.toHaveAttribute('aria-current');

		// A row is a button that moves the counter to it.
		await page.getByRole('button', { name: /Rumours/ }).click();
		expect(onselect).toHaveBeenCalledWith(expect.objectContaining({ key: 'rg-3' }));
	});
});
