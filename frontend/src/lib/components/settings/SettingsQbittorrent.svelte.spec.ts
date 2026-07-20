import { page } from '@vitest/browser/context';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const saveMutate = vi.fn().mockResolvedValue({});
vi.mock('$lib/queries/downloads/ProwlarrTorrentQueries.svelte', () => ({
	getQbittorrentConfigQuery: () => ({
		data: {
			enabled: false,
			client_type: 'qbittorrent',
			url: 'http://qbt:8080',
			api_key: 'qbittorrent****',
			category: 'droppedneedle',
			downloads_mount: '/qbittorrent-downloads'
		},
		isLoading: false,
		isError: false
	}),
	getProwlarrConfigQuery: () => ({ data: { enabled: true }, isLoading: false }),
	saveQbittorrentConfig: () => ({ mutateAsync: saveMutate, isPending: false }),
	testQbittorrent: () => ({ mutateAsync: vi.fn(), isPending: false })
}));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));
import SettingsQbittorrent from './SettingsQbittorrent.svelte';

it('renders and saves the masked qBittorrent API key', async () => {
	render(SettingsQbittorrent);
	await page.getByRole('button', { name: 'Expand' }).click();
	await expect.element(page.getByLabelText('API key', { exact: true })).toHaveValue(
		'qbittorrent****'
	);
	await page.getByRole('button', { name: 'Save settings' }).click();
	expect(saveMutate).toHaveBeenCalledWith(expect.objectContaining({ api_key: 'qbittorrent****' }));
});
