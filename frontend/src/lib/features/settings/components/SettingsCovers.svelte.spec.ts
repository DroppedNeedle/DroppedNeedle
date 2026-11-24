import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { SECRET_MASK } from '$lib/ui/secretMask';

import type { AdvancedSettings } from './advanced-settings-types';
import SettingsCovers from './SettingsCovers.svelte';

const READ = 'GET /api/v3/settings/advanced';
const SAVE = 'PUT /api/v3/settings/advanced';

const stored: AdvancedSettings = {
	audiodb_api_key: SECRET_MASK,
	audiodb_enabled: true,
	audiodb_name_search_fallback: false,
	direct_remote_images_enabled: true,
	discover_blend_listening_services: true,
	discover_picks_count: 12,
	discover_queue_auto_generate: true,
	discover_queue_lastfm_mbid_max_lookups: 10,
	discover_queue_size: 10,
	discover_queue_warm_cycle_build: true,
	ignored_releases_retention_days: 365,
	prefer_local_cover_art: true,
	request_history_retention_days: 180,
	store_prune_interval_hours: 6
};

describe('SettingsCovers', () => {
	it('saves the switch off with every other advanced value and the AudioDB key mask', async () => {
		const api = fakeApi({ [READ]: stored, [SAVE]: ({ body }) => body as AdvancedSettings });
		await renderWithQuery(SettingsCovers, {});

		const local = page.getByRole('checkbox', { name: 'Prefer local cover art' });
		await expect.element(local).toBeChecked();
		await local.click();
		await expect.element(page.getByRole('button', { name: 'Save' })).toBeEnabled();
		await page.getByRole('button', { name: 'Save' }).click();
		await expect.element(page.getByText('Saved.', { exact: true })).toBeVisible();

		const body = api.calls(SAVE)[0].body as AdvancedSettings;
		expect(body).toEqual({ ...stored, prefer_local_cover_art: false });
		expect(body.audiodb_api_key).toBe(SECRET_MASK);
	});
});
