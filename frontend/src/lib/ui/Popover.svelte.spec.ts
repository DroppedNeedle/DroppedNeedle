import { page, userEvent } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import '../../app.css';
import PopoverTestHarness from './PopoverTestHarness.svelte';

const opener = () => page.getByRole('button', { name: /Filter/ });
const panel = () => page.getByRole('dialog', { name: 'Filters' });

describe('Popover', () => {
	it('opens with focus on its first control, and closes on Escape or a tab out', async () => {
		{
			// Opens with focus on its first control and names the panel it opened.
			await render(PopoverTestHarness);
			await expect.element(opener()).not.toHaveAttribute('aria-controls');

			await opener().click();

			await expect.element(panel()).toBeVisible();
			await expect.element(page.getByRole('textbox', { name: 'Minimum' })).toHaveFocus();
			await expect.element(opener()).toHaveAttribute('aria-expanded', 'true');
			await expect.element(opener()).toHaveAttribute('aria-controls');
		}

		{
			// Closes on Escape with focus back on the button.
			cleanup();
			await render(PopoverTestHarness);
			await opener().click();
			await expect.element(panel()).toBeVisible();

			await userEvent.keyboard('{Escape}');

			await expect.element(panel()).not.toBeInTheDocument();
			await expect.element(opener()).toHaveFocus();
		}

		{
			// Closes when focus tabs out of it.
			cleanup();
			await render(PopoverTestHarness);
			await opener().click();
			await expect.element(panel()).toBeVisible();

			await userEvent.tab();
			await userEvent.tab();

			await expect.element(page.getByRole('button', { name: 'Next on the page' })).toHaveFocus();
			await expect.element(panel()).not.toBeInTheDocument();
		}
	});
});
