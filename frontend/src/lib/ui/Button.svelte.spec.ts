import { page } from 'vitest/browser';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import AsyncButton from './AsyncButton.svelte';
import Button from './Button.svelte';
import ButtonTestHarness from './ButtonTestHarness.svelte';

const text = (label: string) => createRawSnippet(() => ({ render: () => `<span>${label}</span>` }));

describe('Button', () => {
	it('does not submit its form unless it is a submit button', async () => {
		const plain = await render(ButtonTestHarness);
		await page.getByRole('button', { name: 'Save' }).click();
		await expect.element(page.getByText('Submitted 0 times')).toBeVisible();
		plain.unmount();

		await render(ButtonTestHarness, { type: 'submit' });
		await page.getByRole('button', { name: 'Save' }).click();
		await expect.element(page.getByText('Submitted 1 times')).toBeVisible();
	});

	it('refuses clicks while busy, loading or awaiting its first promise', async () => {
		const loading = await render(Button, { loading: true, children: text('Saving') });
		const saving = page.getByRole('button', { name: 'Saving' });
		await expect.element(saving).toBeDisabled();
		await expect.element(saving).toHaveAttribute('aria-busy', 'true');
		loading.unmount();

		let finish = () => {};
		const onclick = vi.fn(() => new Promise<void>((resolve) => (finish = resolve)));
		await render(AsyncButton, { onclick, children: text('Sync now') });
		const button = page.getByRole('button', { name: 'Sync now' });

		await button.click();
		await expect.element(button).toBeDisabled();
		await button.click({ force: true });
		expect(onclick).toHaveBeenCalledOnce();

		finish();
		await expect.element(button).toBeEnabled();
	});
});
