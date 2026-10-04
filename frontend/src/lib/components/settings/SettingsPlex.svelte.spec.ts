import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

const h = vi.hoisted(() => ({
	load: vi.fn(),
	save: vi.fn(),
	test: vi.fn(),
	cleanup: vi.fn(),
	start: vi.fn(),
	poll: vi.fn(),
	resetScrobble: vi.fn(),
	getLibraries: vi.fn(),
	formData: {
		plex_url: 'http://plex-server:32400',
		plex_token: 'existing-token',
		music_library_ids: ['1'],
		scrobble_to_plex: true,
		enabled: false,
		login_enabled: false
	}
}));

vi.mock('$lib/utils/settingsForm.svelte', () => ({
	createSettingsForm: () => ({
		data: h.formData,
		loading: false,
		saving: false,
		testing: false,
		message: '',
		messageType: 'success',
		testResult: null,
		wasAlreadyEnabled: false,
		wasAlreadySecondaryEnabled: false,
		load: h.load,
		save: h.save,
		test: h.test,
		cleanup: h.cleanup
	})
}));

vi.mock('$lib/queries/plex/PlexFlowApi', () => ({
	startPlexFlow: (...args: unknown[]) => h.start(...args),
	pollPlexFlow: (...args: unknown[]) => h.poll(...args)
}));

vi.mock('$lib/player/plexPlaybackApi', () => ({
	resetPlexScrobblePreference: (...args: unknown[]) => h.resetScrobble(...args)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: (...args: unknown[]) => h.getLibraries(...args) } } },
	ApiError: class ApiError extends Error {}
}));

import SettingsPlex from './SettingsPlex.svelte';

describe('SettingsPlex connect flow', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		h.start.mockResolvedValue({ pin_id: 7, authorize_url: 'https://app.plex.tv/auth#code=abc' });
		h.poll.mockResolvedValue({ completed: false });
		h.getLibraries.mockResolvedValue([
			{ key: '1', title: 'Music' },
			{ key: '2', title: 'Audiobooks' }
		]);
		vi.spyOn(window, 'open').mockReturnValue(null);
	});

	it('opens the Plex sign-in page and waits for authorization', async () => {
		await render(SettingsPlex);

		await page.getByRole('button', { name: 'Sign in with Plex' }).click();

		expect(h.start).toHaveBeenCalledWith('connect');
		expect(window.open).toHaveBeenCalledWith(
			'https://app.plex.tv/auth#code=abc',
			'_blank',
			'noopener'
		);
		await expect.element(page.getByText('Finish signing in to Plex to continue.')).toBeVisible();
		await expect.element(page.getByRole('link', { name: 'Open sign-in page' })).toBeVisible();
	});

	it('backs out of a pending sign-in without touching the saved token', async () => {
		await render(SettingsPlex);

		await page.getByRole('button', { name: 'Sign in with Plex' }).click();
		await page.getByRole('button', { name: 'Cancel' }).click();

		await expect.element(page.getByRole('button', { name: 'Sign in with Plex' })).toBeVisible();
		await expect
			.element(page.getByText('Finish signing in to Plex to continue.'))
			.not.toBeInTheDocument();
		expect(h.poll).not.toHaveBeenCalled();
	});

	it('lists music libraries once credentials are present', async () => {
		await render(SettingsPlex);

		await expect.element(page.getByText('Music', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Audiobooks', { exact: true })).toBeVisible();
	});

	it('saves through the shared form and resets the scrobble preference', async () => {
		await render(SettingsPlex);

		await page.getByRole('button', { name: 'Save settings' }).click();

		expect(h.save).toHaveBeenCalledOnce();
		expect(h.resetScrobble).toHaveBeenCalledOnce();
	});
});
