import { page } from 'vitest/browser';
import { Trash2 } from 'lucide-svelte';
import { createRawSnippet } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import AsyncButton from './AsyncButton.svelte';
import Button from './Button.svelte';
import ButtonTestHarness from './ButtonTestHarness.svelte';
import IconButton from './IconButton.svelte';

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

	it('renders a link named by its label when given an href', async () => {
		await render(Button, { href: '/library', children: text('Open library') });

		const link = page.getByRole('link', { name: 'Open library' });
		await expect.element(link).toHaveAttribute('href', '/library');
		await expect.element(page.getByRole('button')).not.toBeInTheDocument();
	});

	it('is disabled and busy while loading', async () => {
		await render(Button, { loading: true, children: text('Saving') });

		const button = page.getByRole('button', { name: 'Saving' });
		await expect.element(button).toBeDisabled();
		await expect.element(button).toHaveAttribute('aria-busy', 'true');
	});
});

describe('IconButton', () => {
	it('is a button named by its label', async () => {
		const onclick = vi.fn();
		await render(IconButton, { label: 'Remove track', icon: Trash2, onclick });

		await page.getByRole('button', { name: 'Remove track' }).click();
		expect(onclick).toHaveBeenCalledOnce();
	});
});

describe('AsyncButton', () => {
	it('refuses a second click while its first promise runs', async () => {
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
