import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	load: vi.fn(),
	save: vi.fn(),
	cleanup: vi.fn(),
	options: null as null | { loadEndpoint: string; saveEndpoint: string }
}));

vi.mock('$lib/utils/settingsForm.svelte', () => ({
	createSettingsForm: (options: { loadEndpoint: string; saveEndpoint: string }) => {
		h.options = options;
		return {
			data: { mode: 'review', compilation_policy: 'flag' },
			loading: false,
			saving: false,
			message: '',
			messageType: 'success',
			load: h.load,
			save: h.save,
			cleanup: h.cleanup
		};
	}
}));

import SettingsPlaylistTrackRequests from './SettingsPlaylistTrackRequests.svelte';

describe('SettingsPlaylistTrackRequests', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('edits the admin defaults for Request all tracks', async () => {
		await render(SettingsPlaylistTrackRequests);

		expect(h.options).toEqual({
			loadEndpoint: '/api/v1/settings/playlist-track-requests',
			saveEndpoint: '/api/v1/settings/playlist-track-requests'
		});
		expect(h.load).toHaveBeenCalledOnce();
		await expect.element(page.getByRole('combobox', { name: 'Mode' })).toHaveValue('review');
		await expect
			.element(page.getByRole('combobox', { name: 'Where each track is filed' }))
			.toHaveValue('flag');
		await page.getByRole('button', { name: 'Save' }).click();
		expect(h.save).toHaveBeenCalledOnce();
	});
});
