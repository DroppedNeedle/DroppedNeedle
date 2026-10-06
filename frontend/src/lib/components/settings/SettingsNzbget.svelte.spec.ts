import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({});
const testMutate = vi.fn().mockResolvedValue({
	valid: true,
	version: '24.3',
	message: 'NZBGet 24.3',
	categories: ['music', 'droppedneedle'],
	complete_dir: '/downloads/dst'
});
vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getNzbgetConfigQuery: () => ({
		data: {
			enabled: false,
			client_type: 'nzbget',
			url: 'http://nzbget:6789',
			username: 'nzbget',
			password: 'nzbget****',
			category: 'music',
			priority: 0,
			downloads_mount: '/nzbget-downloads'
		},
		isLoading: false,
		isError: false
	}),
	getNzbgetStatusQuery: () => ({
		data: { valid: true, version: '24.3', message: 'NZBGet 24.3' },
		isLoading: false
	}),
	saveNzbgetConfig: () => ({ mutateAsync: saveMutate, isPending: false }),
	testNzbget: () => ({ mutateAsync: testMutate, isPending: false })
}));

// No indexers configured - the NZBGet card should warn the user that Usenet is inert.
vi.mock('$lib/queries/downloads/IndexerQueries.svelte', () => ({
	getIndexersQuery: () => ({ data: [], isLoading: false })
}));

// vi.hoisted: vi.mock is hoisted above plain top-level consts, so the spy has to be too.
const { toastShow } = vi.hoisted(() => ({ toastShow: vi.fn() }));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: toastShow } }));

import SettingsNzbget from './SettingsNzbget.svelte';

describe('SettingsNzbget.svelte', () => {
	it('shows the NZBGet card header with an enable toggle (collapsed by default)', async () => {
		await render(SettingsNzbget);
		await expect.element(page.getByText('NZBGet')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Enable NZBGet download client')).toBeInTheDocument();
	});

	it('reveals URL + control credential inputs when expanded', async () => {
		await render(SettingsNzbget);
		await page.getByRole('button', { name: 'Expand' }).click();
		await expect.element(page.getByPlaceholder('http://nzbget:6789')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Control username')).toBeInTheDocument();
		await expect.element(page.getByPlaceholder('NZBGet control password')).toBeInTheDocument();
	});

	it('runs Test and shows the connected version', async () => {
		await render(SettingsNzbget);
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('button', { name: 'Test connection' }).click();
		expect(testMutate).toHaveBeenCalledWith(
			expect.objectContaining({ url: 'http://nzbget:6789', client_type: 'nzbget' })
		);
		// A successful test lights up both the header status and the result line.
		await expect.element(page.getByText(/Connected/).first()).toBeInTheDocument();
	});

	it('persists immediately when toggled and warns when no indexer is set up', async () => {
		await render(SettingsNzbget);
		await page.getByRole('button', { name: 'Expand' }).click();
		// Flipping the header switch saves on the spot - no need to hit "Save settings".
		await page.getByLabelText('Enable NZBGet download client').click();
		expect(saveMutate).toHaveBeenCalledWith(
			expect.objectContaining({ enabled: true, client_type: 'nzbget' })
		);
		// With no indexers, an enabled NZBGet is inert - the card must say so.
		await expect.element(page.getByText('No indexers configured.')).toBeInTheDocument();
		// Enabling one Usenet client stands the other down, so the toast has to say it
		// rather than leaving the user to notice SABnzbd flip off by itself.
		expect(toastShow).toHaveBeenCalledWith(
			expect.objectContaining({ message: expect.stringContaining('SABnzbd disabled') })
		);
	});

	it('shows live Connected status from the status query without running Test', async () => {
		testMutate.mockClear();
		await render(SettingsNzbget);
		await expect.element(page.getByText(/Connected/).first()).toBeInTheDocument();
		expect(testMutate).not.toHaveBeenCalled();
	});
});
