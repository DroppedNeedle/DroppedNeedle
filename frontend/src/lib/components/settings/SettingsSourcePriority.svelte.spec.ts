import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// Flipped per test: Usenet is one source served by one of two clients, and the row has
// to name whichever is actually enabled.
const clients = { sabnzbd: false, nzbget: false };

vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getSourcePriorityQuery: () => ({
		data: { order: ['soulseek', 'usenet'] },
		isLoading: false,
		isError: false
	}),
	saveSourcePriority: () => ({ mutateAsync: vi.fn().mockResolvedValue({}), isPending: false }),
	getSabnzbdConfigQuery: () => ({ data: { enabled: clients.sabnzbd }, isLoading: false }),
	getNzbgetConfigQuery: () => ({ data: { enabled: clients.nzbget }, isLoading: false })
}));

vi.mock('$lib/queries/plugins/PluginSourceQueries.svelte', () => ({
	getPluginSourcesQuery: () => ({ data: { sources: [] }, isLoading: false })
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import SettingsSourcePriority from './SettingsSourcePriority.svelte';

describe('SettingsSourcePriority.svelte', () => {
	it('names SABnzbd when it is the enabled Usenet client', async () => {
		clients.sabnzbd = true;
		clients.nzbget = false;
		await render(SettingsSourcePriority);
		await expect.element(page.getByText(/·\s*SABnzbd$/)).toBeInTheDocument();
	});

	it('names NZBGet when it is the enabled Usenet client', async () => {
		clients.sabnzbd = false;
		clients.nzbget = true;
		await render(SettingsSourcePriority);
		await expect.element(page.getByText(/·\s*NZBGet$/)).toBeInTheDocument();
	});

	it('prefers SABnzbd when both are enabled, matching the backend', async () => {
		clients.sabnzbd = true;
		clients.nzbget = true;
		await render(SettingsSourcePriority);
		await expect.element(page.getByText(/·\s*SABnzbd$/)).toBeInTheDocument();
	});

	it('names both when neither is enabled yet', async () => {
		clients.sabnzbd = false;
		clients.nzbget = false;
		await render(SettingsSourcePriority);
		await expect.element(page.getByText(/·\s*SABnzbd or NZBGet/)).toBeInTheDocument();
	});

	it('still lists Usenet as a source regardless of which client serves it', async () => {
		clients.sabnzbd = false;
		clients.nzbget = true;
		await render(SettingsSourcePriority);
		await expect.element(page.getByText('Usenet', { exact: true }).first()).toBeInTheDocument();
		await expect.element(page.getByText('Soulseek', { exact: true }).first()).toBeInTheDocument();
	});
});
