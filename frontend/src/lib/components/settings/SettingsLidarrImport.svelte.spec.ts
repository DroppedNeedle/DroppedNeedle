import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const testMutateAsync = vi.fn();
const saveMutateAsync = vi.fn();

// Mutable saved-config shape; tests set it before render. Empty pair mirrors
// the masked backend getter when nothing is stored.
let mockConfig = { url: '', api_key: '' };

vi.mock('$lib/queries/lidarr-import/LidarrImportQueries.svelte', () => ({
	getLidarrImportConfigQuery: () => ({ data: mockConfig }),
	// The embedded sync card renders inside the parent: pending shape keeps it
	// on its skeleton branch so the Test/Save tests are unaffected.
	getLidarrImportCandidatesQuery: () => ({
		data: undefined,
		isPending: true,
		isError: false,
		refetch: vi.fn()
	})
}));

vi.mock('$lib/queries/lidarr-import/LidarrImportMutations.svelte', () => ({
	saveLidarrConfigMutation: () => ({ mutateAsync: saveMutateAsync, isPending: false }),
	testLidarrMutation: () => ({ mutateAsync: testMutateAsync, isPending: false }),
	importFromLidarrMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import SettingsLidarrImport from './SettingsLidarrImport.svelte';

describe('SettingsLidarrImport', () => {
	beforeEach(() => {
		testMutateAsync.mockReset();
		saveMutateAsync.mockReset();
		mockConfig = { url: '', api_key: '' };
	});

	it('shows the connected version on a successful Test', async () => {
		// The backend crafts the full message; the card renders it verbatim.
		testMutateAsync.mockResolvedValue({
			valid: true,
			version: '3.1.3.4968',
			message: 'Connected - Lidarr v3.1.3.4968'
		});
		await render(SettingsLidarrImport);
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
		await render(SettingsLidarrImport);
		await page.getByLabelText('URL').fill('http://lidarr.test');
		await page.getByRole('button', { name: 'Test' }).click();
		await expect.element(page.getByText(/rejected the API key/)).toBeVisible();
	});

	it('shows the locked sync panel when no connection is saved', async () => {
		mockConfig = { url: '', api_key: '' };
		await render(SettingsLidarrImport);
		await expect.element(page.getByRole('heading', { name: 'Artist sync' })).toBeVisible();
		await expect
			.element(page.getByText('Connect Lidarr above to unlock artist sync.'))
			.toBeVisible();
	});

	it('shows the sync card unlocked once a connection is saved', async () => {
		mockConfig = { url: 'http://lidarr.test', api_key: 'lidarr****' };
		await render(SettingsLidarrImport);
		await expect.element(page.getByRole('heading', { name: 'Artist sync' })).toBeVisible();
		await expect.element(page.getByLabelText('Loading monitored artists')).toBeInTheDocument();
	});
});
