import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import WeekShelf, { type WeekShelfItem } from './WeekShelf.svelte';

const cover = createRawSnippet<[WeekShelfItem]>(() => ({ render: () => '<span></span>' }));

describe('WeekShelf', () => {
	it('lists the week by its sentence, each record with its title and what became of it', async () => {
		await render(WeekShelf, {
			label: 'What you did with week 47',
			look: 'paper',
			cover,
			items: [
				{ key: 'a', title: 'SAWAYAMA', state: 'kept' },
				{ key: 'b', title: 'Dreamhouse', state: 'passed' },
				{ key: 'c', title: 'Rumours', state: 'missed' },
				{ key: 'd', title: 'Cape God' }
			]
		});
		const shelf = page.getByRole('list', { name: 'What you did with week 47' });
		const records = shelf.getByRole('listitem');
		expect(records.all()).toHaveLength(4);
		await expect.element(records.nth(0)).toHaveTextContent(/SAWAYAMA\s*, Kept/);
		// A passed record has no sticker but still says so in words.
		await expect.element(records.nth(1)).toHaveTextContent(/Dreamhouse\s*, Passed/);
		await expect.element(records.nth(2)).toHaveTextContent(/Rumours\s*, Didn't arrive/);
		await expect.element(records.nth(3)).toHaveTextContent('Cape God');
	});

	it('names the week on its tab, each record a button saying its state and its plays', async () => {
		const opened: string[] = [];
		const states = ['kept', 'later', 'known', 'passed', undefined, undefined, 'kept', undefined];
		await render(WeekShelf, {
			label: 'Week 46',
			cover,
			tab: {
				week: 46,
				date: '13 Nov 2026',
				tally: '2 kept - 1 later - 1 known - 1 passed - 3 open'
			},
			onopen: (item: WeekShelfItem) => opened.push(item.key),
			items: states.map((state, i) => ({
				key: `k${i}`,
				title: `Album ${i}`,
				artist: `Artist ${i}`,
				state: state as WeekShelfItem['state'],
				plays: i === 0 ? 14 : i === 6 ? 0 : null
			}))
		});
		await expect.element(page.getByText('13 Nov 2026')).toBeVisible();
		await expect
			.element(page.getByText('2 kept - 1 later - 1 known - 1 passed - 3 open'))
			.toBeVisible();
		const shelf = page.getByRole('list', { name: 'Week 46' });
		expect(shelf.getByRole('listitem').all()).toHaveLength(8);
		await expect
			.element(shelf.getByRole('button', { name: 'Album 0 by Artist 0, Kept, Played 14 times' }))
			.toBeVisible();
		await expect
			.element(shelf.getByRole('button', { name: 'Album 3 by Artist 3, Passed' }))
			.toBeVisible();
		// A kept record with no plays yet says nothing about them.
		await expect
			.element(shelf.getByRole('button', { name: 'Album 6 by Artist 6, Kept' }))
			.toBeVisible();
		expect(page.getByText(/Played/).all()).toHaveLength(1);
		await shelf.getByRole('button', { name: /Album 1 by/ }).click();
		expect(opened).toEqual(['k1']);
	});

	it('shows a week of five as five', async () => {
		await render(WeekShelf, {
			label: 'Week 45',
			cover,
			tab: { week: 45, date: '6 Nov 2026', tally: '5 open' },
			items: Array.from({ length: 5 }, (_, i) => ({ key: `k${i}`, title: `Album ${i}` }))
		});
		expect(page.getByRole('list', { name: 'Week 45' }).getByRole('listitem').all()).toHaveLength(5);
	});
});
