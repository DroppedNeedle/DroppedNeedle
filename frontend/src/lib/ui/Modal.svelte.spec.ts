import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import '../../app.css';
import ModalTestHarness from './ModalTestHarness.svelte';

const opener = () => page.getByRole('button', { name: 'Open settings' });
const dialog = () => page.getByRole('dialog', { name: 'Rename playlist' });

describe('Modal', () => {
	it('holds focus inside while open and gives it back to the opener however it closes', async () => {
		{
			// Opens as a named modal dialog with focus inside it.
			await render(ModalTestHarness);
			await expect.element(dialog()).not.toBeInTheDocument();

			await opener().click();

			await expect.element(dialog()).toBeVisible();
			await expect.element(page.getByRole('textbox', { name: 'Playlist name' })).toHaveFocus();
		}

		{
			// Closes on Escape, tells the owner and returns focus to the opener.
			cleanup();
			const onclose = vi.fn();
			await render(ModalTestHarness, { onclose });
			await opener().click();
			await expect.element(dialog()).toBeVisible();

			await userEvent.keyboard('{Escape}');

			await expect.element(page.getByText('Dialog is closed')).toBeVisible();
			await expect.element(dialog()).not.toBeInTheDocument();
			await expect.element(opener()).toHaveFocus();
			expect(onclose).toHaveBeenCalledOnce();
		}

		{
			// Closes when the owner clears open, and returns focus.
			cleanup();
			await render(ModalTestHarness);
			await opener().click();

			await page.getByRole('button', { name: 'Cancel' }).click();

			await expect.element(dialog()).not.toBeInTheDocument();
			await expect.element(opener()).toHaveFocus();
		}

		{
			// Returns focus to the opener when it is removed while open.
			cleanup();
			await render(ModalTestHarness);
			await opener().click();

			await page.getByRole('button', { name: 'Remove dialog' }).click();

			await expect.element(dialog()).not.toBeInTheDocument();
			await expect.element(opener()).toHaveFocus();
		}
	});

	it('stays open on Escape while it is not dismissible', async () => {
		await render(ModalTestHarness, { dismissible: false });
		await opener().click();
		await expect.element(dialog()).toBeVisible();

		await userEvent.keyboard('{Escape}');

		await expect.element(page.getByText('Dialog is open')).toBeVisible();
		await expect.element(dialog()).toBeVisible();
	});

	it('asks its owner instead of closing when the owner wants to confirm first', async () => {
		const onrequestclose = vi.fn();
		await render(ModalTestHarness, { onrequestclose });
		await opener().click();
		await expect.element(dialog()).toBeVisible();

		await userEvent.keyboard('{Escape}');
		// The backdrop sits under the box, so click it directly.
		(page.getByRole('button', { name: 'Close', exact: true }).element() as HTMLElement).click();

		expect(onrequestclose).toHaveBeenCalledTimes(2);
		await expect.element(dialog()).toBeVisible();
	});
});
