import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const testMutateAsync = vi.fn();
const saveMutateAsync = vi.fn();

vi.mock('$lib/queries/lidarr-import/LidarrImportQueries.svelte', () => ({
	getLidarrImportConfigQuery: () => ({ data: { url: '', api_key: '' } })
}));

vi.mock('$lib/queries/lidarr-import/LidarrImportMutations.svelte', () => ({
	saveLidarrConfigMutation: () => ({ mutateAsync: saveMutateAsync, isPending: false }),
	testLidarrMutation: () => ({ mutateAsync: testMutateAsync, isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import SettingsLidarrImport from './SettingsLidarrImport.svelte';

describe('SettingsLidarrImport', () => {
	beforeEach(() => {
		testMutateAsync.mockReset();
		saveMutateAsync.mockReset();
	});

	it('shows the connected version on a successful Test', async () => {
		// The backend crafts the full message; the card renders it verbatim.
		testMutateAsync.mockResolvedValue({
			valid: true,
			version: '3.1.3.4968',
			message: 'Connected - Lidarr v3.1.3.4968'
		});
		render(SettingsLidarrImport);
		await page.getByLabelText('URL').fill('http://lidarr.test');
		await page.getByRole('button', { name: 'Test' }).click();
		await expect.element(page.getByText(/Connected - Lidarr v3.1.3.4968/)).toBeVisible();
	});

	it('shows a friendly message on a bad-key Test', async () => {
		testMutateAsync.mockResolvedValue({
			valid: false,
			message:
				'Lidarr rejected the API key. Check Settings → General → Security → API Key in Lidarr.'
		});
		render(SettingsLidarrImport);
		await page.getByLabelText('URL').fill('http://lidarr.test');
		await page.getByRole('button', { name: 'Test' }).click();
		await expect.element(page.getByText(/rejected the API key/)).toBeVisible();
	});
});
