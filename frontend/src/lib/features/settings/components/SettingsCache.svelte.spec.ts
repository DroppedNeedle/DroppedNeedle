import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => {
	const stats = () => ({
		entries: 42,
		sources: [],
		provider_data_on: true,
		kept_facts: 12,
		cover_images: 0,
		cover_bytes: 0,
		cover_limit_bytes: 1536 * 1024 * 1024,
		cover_limit_automatic: true,
		database_bytes: 0,
		wal_bytes: 0
	});
	return { clear: vi.fn(), stats, data: stats() };
});

vi.mock('$lib/features/settings/queries/AdminCacheQueries.svelte', () => ({
	getCacheStatsQuery: () => ({
		data: h.data,
		isPending: false,
		error: null,
		refetch: vi.fn()
	}),
	createClearCacheMutation: () => ({ mutateAsync: h.clear, isPending: false }),
	createPrecacheRunMutation: () => ({ mutateAsync: vi.fn(), mutate: vi.fn(), isPending: false })
}));

import SettingsCache from './SettingsCache.svelte';

describe('SettingsCache', () => {
	afterEach(() => {
		vi.restoreAllMocks();
		h.clear.mockReset();
		h.data = h.stats();
	});

	it('shows an automatic image limit', async () => {
		await render(SettingsCache);
		await expect.element(page.getByText('Image limit 1.5 GB (automatic)')).toBeInTheDocument();
	});

	it('names the variable that set a fixed image limit', async () => {
		h.data = { ...h.stats(), cover_limit_bytes: 500 * 1024 * 1024, cover_limit_automatic: false };
		await render(SettingsCache);
		await expect
			.element(page.getByText('Image limit 500 MB (set by COVER_CACHE_MAX_SIZE_MB)'))
			.toBeInTheDocument();
	});

	it('shows how many provider facts are kept', async () => {
		await render(SettingsCache);
		await expect.element(page.getByText('12 kept facts', { exact: false })).toBeInTheDocument();
	});

	it('clears a cache only after the confirm, and declining clears nothing', async () => {
		h.clear.mockResolvedValue({ message: 'Cleared 3 cached images' });
		await render(SettingsCache);
		const dialog = page.getByRole('dialog', { name: 'Clear cache' });

		await page.getByRole('button', { name: 'Clear provider data' }).click();
		await dialog.getByRole('button', { name: 'Cancel' }).click();
		await expect.element(dialog).not.toBeInTheDocument();
		expect(h.clear).not.toHaveBeenCalled();

		await page.getByRole('button', { name: 'Clear images' }).click();
		expect(h.clear).not.toHaveBeenCalled();
		await dialog.getByRole('button', { name: 'Clear', exact: true }).click();

		expect(h.clear).toHaveBeenCalledTimes(1);
		expect(h.clear).toHaveBeenCalledWith({ scope: 'covers' });
		await expect.element(page.getByRole('status')).toHaveTextContent('Cleared 3 cached images');
	});
});
