import { page } from '@vitest/browser/context';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({ success: true });
const testMutate = vi.fn().mockResolvedValue({
	valid: true,
	version: '1.32.2.4987',
	message: 'Connected - Prowlarr v1.32.2.4987 with 2 indexer(s)',
	indexer_count: 2
});

vi.mock('$lib/queries/downloads/ProwlarrQueries.svelte', () => ({
	getProwlarrConfigQuery: () => ({
		data: { enabled: false, url: 'http://prowlarr:9696', api_key: 'prowlarr****' },
		isLoading: false,
		isError: false
	}),
	saveProwlarrConfigMutation: () => ({ mutateAsync: saveMutate, isPending: false }),
	testProwlarrMutation: () => ({ mutateAsync: testMutate, isPending: false })
}));

vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));

import SettingsProwlarr from './SettingsProwlarr.svelte';

describe('SettingsProwlarr.svelte', () => {
	it('renders the connection card seeded from the masked config', async () => {
		await render(SettingsProwlarr);
		await expect.element(page.getByText('Prowlarr')).toBeInTheDocument();
		await expect.element(page.getByLabelText('Enabled')).toBeInTheDocument();
		const url = page.getByLabelText('URL');
		await expect.element(url).toHaveValue('http://prowlarr:9696');
		const key = page.getByLabelText('API key');
		await expect.element(key).toHaveValue('prowlarr****');
	});

	it('shows the untouched masked sentinel is preserved (save sends the sentinel back)', async () => {
		await render(SettingsProwlarr);
		await page.getByRole('button', { name: 'Save' }).click();
		expect(saveMutate).toHaveBeenCalledWith(expect.objectContaining({ api_key: 'prowlarr****' }));
	});

	it('runs Test pre-save and renders the indexer count', async () => {
		await render(SettingsProwlarr);
		await page.getByRole('button', { name: 'Test' }).click();
		expect(testMutate).toHaveBeenCalledWith(
			expect.objectContaining({ url: 'http://prowlarr:9696' })
		);
		await expect.element(page.getByText(/2 indexer\(s\)/)).toBeInTheDocument();
	});

	it('explains SABnzbd is still required for downloads', async () => {
		await render(SettingsProwlarr);
		await expect.element(page.getByText(/SABnzbd still downloads them/)).toBeInTheDocument();
	});
});
