import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { SourceItem, SourcePluginInfo } from '$lib/queries/plugins/types';

const h = vi.hoisted(() => ({
	sources: [] as SourcePluginInfo[],
	items: [] as SourceItem[],
	fetch: vi.fn()
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isTrusted() {
			return true;
		}
	}
}));

vi.mock('$lib/queries/plugins/PluginQueries.svelte', () => ({
	getSourcePluginsQuery: () => ({
		get data() {
			return { sources: h.sources };
		}
	}),
	getSourceSearchQuery: () => ({
		get data() {
			return { items: h.items };
		},
		isFetching: false
	})
}));

vi.mock('$lib/queries/plugins/PluginMutations.svelte', () => ({
	fetchFromSourceMutation: () => ({ mutate: h.fetch, isPending: false })
}));

import PluginSourceSearch from './PluginSourceSearch.svelte';

describe('PluginSourceSearch', () => {
	beforeEach(() => {
		h.sources = [];
		h.items = [];
		h.fetch.mockClear();
	});

	it('renders nothing when no source plugin is enabled (DN ships no sources)', async () => {
		const { container } = render(PluginSourceSearch);
		expect(container.textContent?.trim()).toBe('');
	});

	it('shows the search surface and fetches a picked item', async () => {
		h.sources = [{ name: 'lma-source', display_name: 'Live Music Archive', description: '' }];
		h.items = [{ id: 'show-1', title: 'Live at Fox Theater', artist: 'Band', detail: '1977' }];
		render(PluginSourceSearch);

		await expect.element(page.getByText('Find music in your sources')).toBeVisible();
		await expect.element(page.getByText('Live at Fox Theater')).toBeVisible();
		await page.getByRole('button', { name: 'Fetch' }).click();
		expect(h.fetch).toHaveBeenCalledWith({ plugin: 'lma-source', itemId: 'show-1' });
	});
});
