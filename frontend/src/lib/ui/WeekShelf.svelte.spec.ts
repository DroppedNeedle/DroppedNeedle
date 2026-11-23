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
});
