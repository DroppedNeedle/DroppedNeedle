import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { WantedWatcherSettings } from '$lib/types';

const baseSettings: WantedWatcherSettings = {
	enabled: true,
	auto_download_on_find: true,
	watch_partial_albums: true,
	max_checks_per_sweep: 3,
	dormant_after_days: 365
};

const h = vi.hoisted(() => ({
	settings: undefined as unknown,
	mutateAsync: vi.fn()
}));

vi.mock('$lib/queries/downloads/DownloadClientsQueries.svelte', () => ({
	getWantedSettingsQuery: () => ({
		get data() {
			return h.settings;
		}
	}),
	saveWantedSettings: () => ({
		mutateAsync: h.mutateAsync,
		isPending: false
	})
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import SettingsWanted from './SettingsWanted.svelte';

describe('SettingsWanted', () => {
	beforeEach(() => {
		h.settings = { ...baseSettings };
		h.mutateAsync = vi.fn().mockResolvedValue(undefined);
	});

	it('seeds the toggles and numbers from the saved settings', async () => {
		h.settings = {
			...baseSettings,
			enabled: false,
			auto_download_on_find: false,
			max_checks_per_sweep: 5
		};
		render(SettingsWanted);
		await expect
			.element(page.getByRole('checkbox', { name: 'Watch failed requests' }))
			.not.toBeChecked();
		await expect
			.element(
				page.getByRole('checkbox', {
					name: 'Download automatically when a verified copy appears'
				})
			)
			.not.toBeChecked();
		await expect
			.element(page.getByRole('spinbutton', { name: 'Albums checked per sweep' }))
			.toHaveValue(5);
	});

	it('disables the dependent controls while the master toggle is off', async () => {
		h.settings = { ...baseSettings, enabled: false };
		render(SettingsWanted);
		await expect
			.element(page.getByRole('checkbox', { name: 'Also watch albums with missing tracks' }))
			.toBeDisabled();
		await expect
			.element(page.getByRole('spinbutton', { name: 'Go dormant after (days)' }))
			.toBeDisabled();
	});

	it('saves the edited settings', async () => {
		render(SettingsWanted);
		await page
			.getByRole('checkbox', { name: 'Download automatically when a verified copy appears' })
			.click();
		await page.getByRole('button', { name: 'Save' }).click();
		expect(h.mutateAsync).toHaveBeenCalledWith(
			expect.objectContaining({ enabled: true, auto_download_on_find: false })
		);
	});
});
