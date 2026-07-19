import { page } from '@vitest/browser/context';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({});
vi.mock('$lib/queries/downloads/ProwlarrTorrentQueries.svelte', () => ({
	getProwlarrConfigQuery: () => ({
		data: {
			enabled: true,
			url: 'http://prowlarr:9696',
			api_key: 'prowlarr****',
			categories: [3000, 3040]
		},
		isLoading: false,
		isError: false
	}),
	saveProwlarrConfig: () => ({ mutateAsync: saveMutate, isPending: false }),
	testProwlarr: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));
import SettingsProwlarr from './SettingsProwlarr.svelte';

it('preserves editable Prowlarr categories when saving', async () => {
	render(SettingsProwlarr);
	await page.getByRole('button', { name: 'Expand' }).click();
	await expect.element(page.getByLabelText('Categories')).toHaveValue('3000, 3040');
	await page.getByRole('button', { name: 'Save settings' }).click();
	expect(saveMutate).toHaveBeenCalledWith(expect.objectContaining({ categories: [3000, 3040] }));
});
