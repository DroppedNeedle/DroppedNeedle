import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	library: { data: { library_roots: [], acoustid_api_key: '' } } as Record<string, unknown>,
	runHistory: {
		data: { pages: [{ items: [], next_cursor: null }], pageParams: [undefined] }
	} as unknown as Record<string, unknown>
}));

vi.mock('$lib/queries/downloads/DownloadClientQueries.svelte', () => ({
	getDownloadClientConfigQuery: () => ({ data: { url: '', api_key: '' } }),
	getDownloadClientStatusQuery: () => ({ data: { mount: { ok: false } } })
}));
vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getSabnzbdConfigQuery: () => ({ data: { enabled: false, url: '' } })
}));
vi.mock('$lib/queries/downloads/IndexerQueries.svelte', () => ({
	getIndexersQuery: () => ({ data: [] }),
	getSearchBackendQuery: () => ({ data: { backend: 'indexers' } })
}));
vi.mock('$lib/queries/downloads/ProwlarrQueries.svelte', () => ({
	getProwlarrConfigQuery: () => ({ data: { url: '', api_key: '' } })
}));
vi.mock('$lib/queries/library/LibraryPolicyQueries.svelte', () => ({
	getTargetLibrarySettingsQuery: () => h.library
}));
vi.mock('$lib/queries/library/LibraryOperationQueries.svelte', () => ({
	getLibraryRunHistoryQuery: () => h.runHistory
}));

import SettingsOnboardingChecklist from './SettingsOnboardingChecklist.svelte';

function historyWith(
	items: Array<{ state: string; terminal_at: number | null }>
): Record<string, unknown> {
	return {
		data: { pages: [{ items, next_cursor: null }], pageParams: [undefined] }
	} as unknown as Record<string, unknown>;
}

beforeEach(() => {
	vi.clearAllMocks();
	h.library = { data: { library_roots: [], acoustid_api_key: '' } };
	h.runHistory = historyWith([]);
});

describe('SettingsOnboardingChecklist', () => {
	it('leaves the scan item undone before any run exists', async () => {
		await render(SettingsOnboardingChecklist);
		await expect.element(page.getByText('Run a library scan')).toBeVisible();
		await expect.element(page.getByText('0/5 complete')).toBeVisible();
	});

	it('leaves the scan item undone while the first scan is still running', async () => {
		h.runHistory = historyWith([{ state: 'indexing', terminal_at: null }]);
		await render(SettingsOnboardingChecklist);
		await expect.element(page.getByText('Run a library scan')).toBeVisible();
		await expect.element(page.getByText('0/5 complete')).toBeVisible();
	});

	it('marks the scan item done after the first terminal run', async () => {
		h.runHistory = historyWith([{ state: 'completed', terminal_at: 1_700_000_000 }]);
		await render(SettingsOnboardingChecklist);
		await expect.element(page.getByText('Run a library scan')).toBeVisible();
		await expect.element(page.getByText('1/5 complete')).toBeVisible();
	});
});
