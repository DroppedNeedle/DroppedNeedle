import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { themeStore } from '$lib/stores/theme.svelte';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import TopBar from './TopBar.svelte';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

describe('TopBar theme toggle', () => {
	afterEach(() => {
		themeStore.set('dark');
	});

	it('switches to the other theme and renames itself', async () => {
		fakeApi({ 'GET /api/v3/system/health': { degraded: [] } });
		themeStore.set('dark');
		await renderWithQuery(TopBar, { onsearch: () => {} });

		await page.getByRole('button', { name: 'Switch to light theme' }).click();

		expect(document.documentElement.dataset.theme).toBe('light');
		await expect.element(page.getByRole('button', { name: 'Switch to dark theme' })).toBeVisible();
	});
});
