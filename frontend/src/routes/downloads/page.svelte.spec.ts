import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	loaded: true,
	configured: false,
	isAdmin: false,
	isTrusted: false
}));

vi.mock('$lib/stores/integration', () => ({
	integrationStore: {
		subscribe: (run: (v: { loaded: boolean; download_client: boolean }) => void) => {
			run({ loaded: h.loaded, download_client: h.configured });
			return () => {};
		},
		ensureLoaded: vi.fn()
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAdmin() {
			return h.isAdmin;
		},
		get isTrusted() {
			return h.isTrusted;
		},
		get user() {
			return { id: 'user-1' };
		}
	}
}));

vi.mock('$lib/queries/import/DropImportQueries.svelte', () => ({
	getDropImportJobsQuery: () => ({ data: { jobs: [] }, isLoading: false })
}));

vi.mock('$lib/queries/import/DropImportMutations.svelte', () => ({
	uploadDropMutation: () => ({ mutate: vi.fn(), isPending: false }),
	matchDropItemMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	discardDropItemMutation: () => ({ mutate: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/plugins/PluginQueries.svelte', () => ({
	getSourcePluginsQuery: () => ({ data: { sources: [] }, isLoading: false }),
	getSourceSearchQuery: () => ({ data: { items: [] }, isFetching: false })
}));

vi.mock('$lib/queries/plugins/PluginMutations.svelte', () => ({
	fetchFromSourceMutation: () => ({ mutate: vi.fn(), isPending: false })
}));

import DownloadsPage from './+page.svelte';

describe('/downloads page', () => {
	beforeEach(() => {
		h.loaded = true;
		h.configured = false;
		h.isAdmin = false;
		h.isTrusted = false;
	});

	it('shows the admin setup CTA when the client is not configured', async () => {
		h.isAdmin = true;
		h.isTrusted = true;
		render(DownloadsPage);
		await expect.element(page.getByText('Download client not configured')).toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Configure Download Client' }))
			.toBeVisible();
	});

	it('shows a non-admin message (no CTA) when not configured', async () => {
		h.isAdmin = false;
		render(DownloadsPage);
		await expect
			.element(page.getByText('Contact your admin to configure the download client.'))
			.toBeVisible();
		await expect
			.element(page.getByRole('link', { name: 'Configure Download Client' }))
			.not.toBeInTheDocument();
	});

	it('shows a loading skeleton before integration status loads', async () => {
		h.loaded = false;
		const { container } = render(DownloadsPage);
		expect(container.querySelector('.skeleton')).not.toBeNull();
	});

	it('hides the Import tab from plain users', async () => {
		h.isTrusted = false;
		render(DownloadsPage);
		await expect.element(page.getByText('Download client not configured')).toBeVisible();
		await expect.element(page.getByRole('tab', { name: 'Import' })).not.toBeInTheDocument();
	});

	it('lets a curator switch to the Import tab and see the drop zone', async () => {
		h.isTrusted = true;
		render(DownloadsPage);
		await page.getByRole('tab', { name: 'Import' }).click();
		await expect.element(page.getByText('Drop your purchases here')).toBeVisible();
	});

	it('shows the everyone toggle only to admins on the Import tab', async () => {
		h.isTrusted = true;
		h.isAdmin = true;
		render(DownloadsPage);
		await page.getByRole('tab', { name: 'Import' }).click();
		await expect.element(page.getByText("Show everyone's imports")).toBeVisible();
	});
});
