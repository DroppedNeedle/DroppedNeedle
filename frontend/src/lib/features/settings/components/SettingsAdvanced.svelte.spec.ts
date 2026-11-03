import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { apiError, fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { SECRET_MASK } from '$lib/ui/secretMask';

import type { AdvancedSettings } from './advanced-settings-types';
import SettingsAdvanced from './SettingsAdvanced.svelte';

const READ = 'GET /api/v3/settings/advanced';
const SAVE = 'PUT /api/v3/settings/advanced';

const stored: AdvancedSettings = {
	audiodb_api_key: SECRET_MASK,
	audiodb_enabled: true,
	audiodb_name_search_fallback: false,
	direct_remote_images_enabled: true,
	discover_blend_listening_services: true,
	discover_picks_count: 12,
	discover_picks_genre_affinity_weight: 0.7,
	discover_queue_albums_per_similar: 5,
	discover_queue_auto_generate: true,
	discover_queue_lastfm_mbid_max_lookups: 10,
	discover_queue_seed_artists: 3,
	discover_queue_similar_artists_limit: 15,
	discover_queue_size: 10,
	discover_queue_warm_cycle_build: true,
	discover_queue_wildcard_slots: 2,
	ignored_releases_retention_days: 365,
	prefer_local_cover_art: true,
	request_history_retention_days: 180,
	store_prune_interval_hours: 6
};

describe('SettingsAdvanced retention', () => {
	it('shows the retention settings in their units and bounds, and saves them with the other values', async () => {
		const api = fakeApi({ [READ]: stored, [SAVE]: ({ body }) => body as AdvancedSettings });
		await renderWithQuery(SettingsAdvanced, {});
		await page.getByRole('radio', { name: 'Retention' }).click();

		const history = page.getByRole('spinbutton', { name: 'Request History (days)' });
		await expect.element(history).toHaveValue(180);
		await expect.element(history).toHaveAttribute('min', '30');
		await expect.element(history).toHaveAttribute('max', '3650');
		await expect
			.element(page.getByRole('spinbutton', { name: 'Ignored Albums (days)' }))
			.toHaveValue(365);
		const interval = page.getByRole('spinbutton', { name: 'Clean-up Interval (hours)' });
		await expect.element(interval).toHaveValue(6);
		await expect.element(interval).toHaveAttribute('max', '168');

		await history.fill('90');
		await page.getByRole('button', { name: 'Save' }).click();
		await expect
			.element(page.getByText('Saved. Some changes apply after a page reload.'))
			.toBeVisible();
		expect(api.calls(SAVE)[0].body).toEqual({ ...stored, request_history_retention_days: 90 });
	});

	it("shows the server's reason beside a refused retention value", async () => {
		fakeApi({
			[READ]: stored,
			[SAVE]: apiError(
				400,
				'INVALID_INPUT',
				'Request history retention days: must be between 30 and 3650, got 5',
				{ field: 'request_history_retention_days', reason: 'must be between 30 and 3650, got 5' }
			)
		});
		await renderWithQuery(SettingsAdvanced, {});
		await page.getByRole('radio', { name: 'Retention' }).click();

		await page.getByRole('spinbutton', { name: 'Request History (days)' }).fill('5');
		await page.getByRole('button', { name: 'Save' }).click();
		await expect
			.element(page.getByText('must be between 30 and 3650, got 5', { exact: true }))
			.toBeVisible();
	});
});
