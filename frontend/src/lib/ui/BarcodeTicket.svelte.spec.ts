import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import BarcodeTicket from './BarcodeTicket.svelte';

describe('BarcodeTicket', () => {
	it('prints facts as text, and keeps the ornament hidden as before', async () => {
		{
			// The week ticket shows its figure and two lines, and is heard as one sentence.
			await render(BarcodeTicket, {
				figure: '47',
				lines: ['Week', 'Set out Fri 20 Nov'],
				spoken: 'Week 47, set out Friday 20 November'
			});
			await expect.element(page.getByText('47', { exact: true })).toBeVisible();
			await expect.element(page.getByText('Set out Fri 20 Nov')).toBeVisible();
			await expect
				.element(page.getByText('Week 47, set out Friday 20 November'))
				.toBeInTheDocument();
		}

		{
			// The ornament is decoration: its label is hidden from a screen reader.
			cleanup();
			await render(BarcodeTicket, { label: 'DN / CC 3.2' });
			const ornament = page.getByText('DN / CC 3.2').element().closest('.dn-ticket');
			await expect.element(ornament as HTMLElement).toHaveAttribute('aria-hidden', 'true');
		}
	});
});
