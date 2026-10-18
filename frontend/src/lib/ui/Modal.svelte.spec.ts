import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import '../../app.css';
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

	it('as a sheet fills a phone screen, with its action bar on screen', async () => {
		await page.viewport(390, 844);
		await render(ModalTestHarness, {
			size: 'sheet',
			description: 'Pick a name your friends will recognise.'
		});
		await opener().click();

		await expect.element(dialog()).toHaveAccessibleDescription(/friends will recognise/);
		const box = () => page.getByTestId('modal-sheet').element().getBoundingClientRect();
		// The box scales up as it opens.
		await expect.poll(() => box().width).toBe(390);
		expect(box().height).toBe(844);
		await expect.element(page.getByRole('button', { name: 'Cancel' })).toBeInViewport();
	});

	it('as a sheet on a desktop is a centred box shorter than the window', async () => {
		await page.viewport(1440, 900);
		await render(ModalTestHarness, { size: 'sheet' });
		await opener().click();
		await expect.element(dialog()).toBeVisible();

		const box = () => page.getByTestId('modal-sheet').element().getBoundingClientRect();
		await expect.poll(() => box().width).toBe(1152);
		expect(box().height).toBeLessThanOrEqual(900 * 0.88);
		expect(box().left).toBe((1440 - 1152) / 2);
	});

	it('returns focus to the opener when it is removed while open', async () => {
		await render(ModalTestHarness);
		await opener().click();

		await page.getByRole('button', { name: 'Remove dialog' }).click();

		await expect.element(dialog()).not.toBeInTheDocument();
		await expect.element(opener()).toHaveFocus();
	});
});
