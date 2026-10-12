import { page, userEvent } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import ModalTestHarness from './ModalTestHarness.svelte';

const opener = () => page.getByRole('button', { name: 'Open settings' });
const dialog = () => page.getByRole('dialog', { name: 'Rename playlist' });

describe('Modal', () => {
	it('opens as a named modal dialog with focus inside it', async () => {
		await render(ModalTestHarness);
		await expect.element(dialog()).not.toBeInTheDocument();

		await opener().click();

		await expect.element(dialog()).toBeVisible();
		await expect.element(page.getByRole('textbox', { name: 'Playlist name' })).toHaveFocus();
	});

	it('closes on Escape, tells the owner and returns focus to the opener', async () => {
		const onclose = vi.fn();
		await render(ModalTestHarness, { onclose });
		await opener().click();
		await expect.element(dialog()).toBeVisible();

		await userEvent.keyboard('{Escape}');

		await expect.element(page.getByText('Dialog is closed')).toBeVisible();
		await expect.element(dialog()).not.toBeInTheDocument();
		await expect.element(opener()).toHaveFocus();
		expect(onclose).toHaveBeenCalledOnce();
	});

	it('closes when the owner clears open, and returns focus', async () => {
		await render(ModalTestHarness);
		await opener().click();

		await page.getByRole('button', { name: 'Cancel' }).click();

		await expect.element(dialog()).not.toBeInTheDocument();
		await expect.element(opener()).toHaveFocus();
	});

	it('stays open on Escape while it is not dismissible', async () => {
		await render(ModalTestHarness, { dismissible: false });
		await opener().click();
		await expect.element(dialog()).toBeVisible();

		await userEvent.keyboard('{Escape}');

		await expect.element(page.getByText('Dialog is open')).toBeVisible();
		await expect.element(dialog()).toBeVisible();
	});

	it('returns focus to the opener when it is removed while open', async () => {
		await render(ModalTestHarness);
		await opener().click();

		await page.getByRole('button', { name: 'Remove dialog' }).click();

		await expect.element(dialog()).not.toBeInTheDocument();
		await expect.element(opener()).toHaveFocus();
	});
});
