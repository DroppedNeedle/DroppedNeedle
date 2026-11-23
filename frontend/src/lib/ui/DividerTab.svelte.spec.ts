import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import DividerTab from './DividerTab.svelte';

describe('DividerTab', () => {
	it('shows the name on the tab and the share apart from it', async () => {
		await render(DividerTab, {
			name: 'Around Poppy',
			share: '35%',
			shareSpoken: '35% of your listening'
		});
		// The tab carries the name and nothing else.
		const tab = page.getByText('Around Poppy', { exact: true });
		await expect.element(tab).toBeVisible();
		// The share is printed outside the tab, at the line's end.
		const share = page.getByText('35%', { exact: true });
		await expect.element(share).toBeVisible();
		expect(tab.element().contains(share.element())).toBe(false);
		// A screen reader hears the share as words, after the name.
		await expect.element(page.getByText(', 35% of your listening')).toBeInTheDocument();
	});
});
