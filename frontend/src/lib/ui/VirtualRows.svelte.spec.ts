import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { createRawSnippet } from 'svelte';

import VirtualRows from './VirtualRows.svelte';

const loaded = (upTo: number) =>
	createRawSnippet((index: () => number) => ({
		render: () =>
			index() < upTo
				? `<div role="cell"><button type="button">Row ${index()}</button></div>`
				: `<div role="cell" data-placeholder tabindex="-1" aria-busy="true">Loading</div>`
	}));

const row = createRawSnippet((index: () => number) => ({
	render: () => `<div role="cell"><button type="button">Row ${index()}</button></div>`
}));

afterEach(() => window.scrollTo(0, 0));

describe('VirtualRows', () => {
	it('moves focus by position and keeps the focused row mounted', async () => {
		await render(VirtualRows, { count: 10000, pitch: 40, label: 'Rows', item: row });
		const table = page.getByRole('table', { name: 'Rows' });
		await expect.element(table).toHaveAttribute('aria-rowcount', '10000');

		await page.getByRole('button', { name: 'Row 0', exact: true }).click();
		await userEvent.keyboard('{ArrowDown}');
		await expect.element(page.getByRole('button', { name: 'Row 1', exact: true })).toHaveFocus();

		await userEvent.keyboard('{End}');
		// The jump scrolls the page, and the moved rows paint on a later frame.
		const focused = () => document.activeElement?.textContent;
		await expect.poll(focused, { timeout: 5000, interval: 100 }).toBe('Row 9999');
		expect(document.activeElement?.closest('[role="row"]')?.getAttribute('aria-rowindex')).toBe(
			'10000'
		);

		// Scrolled back to the top, the focused row is still there.
		window.scrollTo(0, 0);
		await expect
			.poll(() => document.querySelector('[data-index="0"]'), { timeout: 5000, interval: 100 })
			.not.toBeNull();
		expect(focused()).toBe('Row 9999');
		await userEvent.keyboard('{Home}');
		await expect.poll(focused, { timeout: 5000, interval: 100 }).toBe('Row 0');
	});

	it('keeps focus on a row that has not loaded and hands it over once it does', async () => {
		const view = await render(VirtualRows, {
			count: 10000,
			pitch: 40,
			label: 'Rows',
			item: loaded(50)
		});
		await page.getByRole('button', { name: 'Row 0', exact: true }).click();
		await userEvent.keyboard('{End}');
		const focusedRow = () =>
			document.activeElement?.closest('[role="row"]')?.getAttribute('aria-rowindex');
		await expect.poll(focusedRow, { timeout: 5000, interval: 100 }).toBe('10000');

		// Its block arrives: the row's own target takes the focus.
		view.rerender({ item: loaded(10000) });
		const focused = () => document.activeElement?.textContent;
		await expect.poll(focused, { timeout: 5000, interval: 100 }).toBe('Row 9999');
	});
});
