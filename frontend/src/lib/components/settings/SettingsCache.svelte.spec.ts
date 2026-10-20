import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({ clear: vi.fn() }));

vi.mock('$lib/queries/settings/AdminCacheQueries.svelte', () => ({
	getCacheStatsQuery: () => ({
		data: { entries: 42, sources: [], provider_data_on: true, cover_images: 0, cover_bytes: 0 },
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
	});

	it('clears the images after confirmation', async () => {
		h.clear.mockResolvedValue({ message: 'Cleared 3 cached images' });

		await render(SettingsCache);
		await page.getByRole('button', { name: 'Clear images' }).click();
		expect(h.clear).not.toHaveBeenCalled();

		const dialog = page.getByRole('dialog', { name: 'Clear cache' });
		await dialog.getByRole('button', { name: 'Clear', exact: true }).click();

		expect(h.clear).toHaveBeenCalledTimes(1);
		expect(h.clear).toHaveBeenCalledWith({ scope: 'covers' });
		await expect.element(page.getByRole('status')).toHaveTextContent('Cleared 3 cached images');
	});

	it('does nothing when the confirmation is declined', async () => {
		await render(SettingsCache);
		await page.getByRole('button', { name: 'Clear everything' }).click();
		await page
			.getByRole('dialog', { name: 'Clear cache' })
			.getByRole('button', { name: 'Cancel' })
			.click();

		await expect.element(page.getByRole('dialog', { name: 'Clear cache' })).not.toBeInTheDocument();
		expect(h.clear).not.toHaveBeenCalled();
	});
});
