import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import Receipt from './Receipt.svelte';

describe('Receipt', () => {
	it('reads as a table: each value beside its label, under its column', async () => {
		await render(Receipt, {
			title: 'Discover',
			subtitle: 'Week 47 - set out Fri 20 Nov 2026',
			sections: [
				{
					head: 'Made from',
					rows: [
						{ label: 'Your plays, 3 Feb to 20 Nov', values: ['459'] },
						{ label: 'Artists you played twice or more', values: ['23'] }
					]
				},
				{
					head: 'Your tastes',
					columns: ['Share', 'Picks'],
					rows: [{ label: 'Around Poppy', values: ['35%', '2'] }]
				},
				{ head: 'Picked', value: '8' }
			]
		});
		const table = page.getByRole('table', { name: 'Discover, Week 47 - set out Fri 20 Nov 2026' });
		await expect.element(table).toBeVisible();
		await expect
			.element(table.getByRole('row', { name: 'Your plays, 3 Feb to 20 Nov 459' }))
			.toBeVisible();
		await expect.element(table.getByRole('row', { name: 'Around Poppy 35% 2' })).toBeVisible();
		await expect.element(table.getByRole('columnheader', { name: 'Share' })).toBeVisible();
		await expect.element(table.getByRole('row', { name: 'Picked 8' })).toBeVisible();
	});
});
