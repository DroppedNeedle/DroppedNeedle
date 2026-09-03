import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({});
const testMutate = vi.fn().mockResolvedValue({
	valid: true,
	version: '5.0.4',
	message: 'SABnzbd 5.0.4',
	categories: ['*', 'audio'],
	complete_dir: '/data/Downloads/complete'
});

vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getSabnzbdConfigQuery: () => ({
		data: {
			enabled: false,
			client_type: 'sabnzbd',
			url: 'http://sab:8080',
			api_key: 'sabnzbd****',
			category: '*',
			priority: 0,
			post_processing: 3,
			downloads_mount: '/sabnzbd-downloads'
		},
		isLoading: false,
		isError: false
	}),
	getSabnzbdStatusQuery: () => ({
		data: { valid: true, version: '5.0.4', message: 'SABnzbd 5.0.4' },
		isLoading: false
	}),
	saveSabnzbdConfig: () => ({ mutateAsync: saveMutate, isPending: false }),
	testSabnzbd: () => ({ mutateAsync: testMutate, isPending: false })
}));

// No indexers configured - the SABnzbd card should warn the user that Usenet is inert.
vi.mock('$lib/queries/downloads/IndexerQueries.svelte', () => ({
	getIndexersQuery: () => ({ data: [], isLoading: false })
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import SettingsSabnzbd from './SettingsSabnzbd.svelte';

describe('SettingsSabnzbd.svelte', () => {
	it('shows the SABnzbd card header with an enable toggle (collapsed by default)', async () => {
		render(SettingsSabnzbd);
		await expect.element(page.getByText('SABnzbd')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Enable SABnzbd download client')).toBeInTheDocument();
	});

	it('reveals URL + full-key inputs when expanded', async () => {
		render(SettingsSabnzbd);
		await page.getByRole('button', { name: 'Expand' }).click();
		await expect.element(page.getByPlaceholder('http://sabnzbd:8080')).toBeInTheDocument();
		await expect.element(page.getByPlaceholder('SABnzbd full API key')).toBeInTheDocument();
	});

	it('runs Test and shows the connected version', async () => {
		render(SettingsSabnzbd);
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('button', { name: 'Test connection' }).click();
		expect(testMutate).toHaveBeenCalledWith(
			expect.objectContaining({ url: 'http://sab:8080', client_type: 'sabnzbd' })
		);
		// A successful test lights up both the header status and the result line.
		await expect.element(page.getByText(/Connected/).first()).toBeInTheDocument();
	});

	it('persists immediately when toggled and warns when no indexer is set up', async () => {
		render(SettingsSabnzbd);
		await page.getByRole('button', { name: 'Expand' }).click();
		// Flipping the header switch saves on the spot - no need to hit "Save settings".
		await page.getByLabelText('Enable SABnzbd download client').click();
		expect(saveMutate).toHaveBeenCalledWith(expect.objectContaining({ enabled: true }));
		// With no indexers, an enabled SABnzbd is inert - the card must say so.
		await expect.element(page.getByText('No indexers configured.')).toBeInTheDocument();
	});

	it('shows live Connected status from the status query without running Test', async () => {
		testMutate.mockClear();
		render(SettingsSabnzbd);
		await expect.element(page.getByText(/Connected/).first()).toBeInTheDocument();
		expect(testMutate).not.toHaveBeenCalled();
	});
});
