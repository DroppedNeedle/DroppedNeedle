import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, render } from 'vitest-browser-svelte';

import { toastStore } from '$lib/stores/toast';
import Toaster from './Toaster.svelte';

/** Replaces the page's clipboard for one test. `undefined` is a plain-http page. */
function setClipboard(value: unknown): void {
	Object.defineProperty(navigator, 'clipboard', { value, configurable: true });
}

afterEach(() => {
	cleanup();
	toastStore.hide();
	// Removing the test's own property puts the browser's clipboard back.
	Reflect.deleteProperty(navigator, 'clipboard');
});

it('shows an error ref to select, and Copy writes it when the page has a clipboard', async () => {
	const writeText = vi.fn().mockResolvedValue(undefined);
	setClipboard({ writeText });
	toastStore.show({ message: 'Internal server error (ref 4f1c2a9e)', type: 'error' });
	render(Toaster);

	await expect.element(page.getByText('Internal server error', { exact: true })).toBeVisible();
	await expect.element(page.getByText('4f1c2a9e', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Copy' }).click();
	expect(writeText).toHaveBeenCalledWith('4f1c2a9e');
});

it('shows the ref without a Copy button where the page has no clipboard', async () => {
	setClipboard(undefined);
	toastStore.show({ message: 'Internal server error (ref 4f1c2a9e)', type: 'error' });
	render(Toaster);

	const ref = page.getByText('4f1c2a9e', { exact: true });
	await expect.element(ref).toBeVisible();
	await expect.element(page.getByRole('button', { name: 'Copy' })).not.toBeInTheDocument();
	// The negative check polls, and a toast that closes meanwhile would pass it,
	// so the ref must still be on screen after it.
	await expect.element(ref).toBeVisible();
});
