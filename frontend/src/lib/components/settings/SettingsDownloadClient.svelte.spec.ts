import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({});

const testMutate = vi.fn().mockResolvedValue({
	valid: true,
	version: '0.25.1.0',
	message: 'slskd 0.25.1.0'
});

vi.mock('$lib/queries/downloads/DownloadClientQueries.svelte', () => ({
	getDownloadClientConfigQuery: () => ({
		data: {
			enabled: false,
			client_type: 'slskd',
			url: 'http://slskd:5030',
			api_key: 'slskd****',
			verify_downloads: true,
			min_bitrate_kbps: 128,
			preflight_score_auto_accept: 0.7,
			preflight_score_manual_min: 0.5
		},
		isLoading: false,
		isError: false
	}),
	getDownloadClientStatusQuery: () => ({
		data: {
			// Non-"Connected" status header so the only /Connected/ match is the test result line below.
			configured: true,
			client: { status: 'error', version: null, message: 'Not reachable' },
			mount: { ok: true, move_supported: true, reason: 'ok', path: '/data/downloads/slskd' }
		}
	}),
	saveDownloadClientConfig: () => ({
		mutateAsync: saveMutate,
		isPending: false
	}),
	testDownloadClient: () => ({ mutateAsync: testMutate, isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import SettingsDownloadClient from './SettingsDownloadClient.svelte';

describe('SettingsDownloadClient.svelte', () => {
	it('shows the slskd card header with an enable toggle (collapsed by default)', async () => {
		render(SettingsDownloadClient);
		await expect.element(page.getByText('slskd')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Enable slskd download client')).toBeInTheDocument();
	});

	it('reveals the URL and API key inputs when expanded', async () => {
		render(SettingsDownloadClient);
		await page.getByRole('button', { name: 'Expand' }).click();
		await expect.element(page.getByPlaceholder('http://slskd:5030')).toBeInTheDocument();
		await expect.element(page.getByPlaceholder('slskd API key')).toBeInTheDocument();
	});

	it('runs Test connection with the current form values and shows the result', async () => {
		render(SettingsDownloadClient);
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('button', { name: 'Test connection' }).click();
		// Test sends the form config, not an empty body, so the backend validates what's typed.
		expect(testMutate).toHaveBeenCalledWith(
			expect.objectContaining({ url: 'http://slskd:5030', api_key: 'slskd****' })
		);
		await expect.element(page.getByText(/Connected/)).toBeInTheDocument();
	});

	it('reveals the incomplete-folder input and sends it on save', async () => {
		render(SettingsDownloadClient);
		await page.getByRole('button', { name: 'Expand' }).click();
		const input = page.getByPlaceholder('e.g. /data/slskd/incomplete');
		await expect.element(input).toBeInTheDocument();
		await input.fill('/data/slskd/incomplete');
		await page.getByRole('button', { name: 'Save settings' }).click();
		expect(saveMutate).toHaveBeenCalledWith(
			expect.objectContaining({ slskd_incomplete_mount: '/data/slskd/incomplete' })
		);
	});
});
