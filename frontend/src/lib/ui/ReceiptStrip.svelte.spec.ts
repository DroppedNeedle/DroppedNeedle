import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import ReceiptStrip from './ReceiptStrip.svelte';

const link = createRawSnippet(() => ({
	render: () => '<button type="button">The receipt</button>'
}));

describe('ReceiptStrip', () => {
	it('lists each figure with its label, in order, with the link at its end', async () => {
		await render(ReceiptStrip, {
			label: 'What the set was made from',
			cells: [
				{ figure: '459', label: 'plays, and your shelves' },
				{ figure: '69', label: 'you know: left out', struck: true },
				{ figure: '8', label: 'picked', total: true }
			],
			link
		});
		const list = page.getByRole('list', { name: 'What the set was made from' });
		const cells = list.getByRole('listitem');
		await expect.element(cells.nth(0)).toHaveTextContent(/459\s*plays, and your shelves/);
		await expect.element(cells.nth(1)).toHaveTextContent(/69\s*you know: left out/);
		await expect.element(cells.nth(2)).toHaveTextContent(/8\s*picked/);
		expect(cells.all()).toHaveLength(3);
		await expect.element(page.getByRole('button', { name: 'The receipt' })).toBeVisible();
	});
});
