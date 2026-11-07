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

	it('cycles dark to midnight to light and renames itself', async () => {
		const api = fakeApi({
			'GET /api/v3/system/health': { degraded: [] },
			'PUT /api/v3/me/appearance': ({ body }) => ({
				theme: (body as { theme: 'dark' | 'light' | 'midnight' }).theme,
				custom_css: { text: '', enabled: true, saved_at: null },
				site: { css: '', allow_user_css: true },
				stored: true
			})
		});
		themeStore.set('dark');
		await renderWithQuery(TopBar, { onsearch: () => {} });

		await page.getByRole('button', { name: 'Switch to midnight theme' }).click();

		expect(document.documentElement.dataset.theme).toBe('midnight');
		await expect
			.poll(() => api.calls('PUT /api/v3/me/appearance').at(-1)?.body)
			.toEqual({ theme: 'midnight' });
		await expect.element(page.getByRole('button', { name: 'Switch to light theme' })).toBeVisible();

		await page.getByRole('button', { name: 'Switch to light theme' }).click();

		expect(document.documentElement.dataset.theme).toBe('light');
		await expect.element(page.getByRole('button', { name: 'Switch to dark theme' })).toBeVisible();
	});
});
