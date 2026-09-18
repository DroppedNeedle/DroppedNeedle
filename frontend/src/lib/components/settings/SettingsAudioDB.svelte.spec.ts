import { page } from '@vitest/browser/context';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { AdvancedSettingsForm } from './advanced-settings-types';
import SettingsAudioDB from './SettingsAudioDB.svelte';

// Only the fields this tile reads; the rest of the shared form is irrelevant here.
const data = {
	audiodb_enabled: true,
	audiodb_api_key: '',
	audiodb_name_search_fallback: true,
	direct_remote_images_enabled: false,
	prefer_local_cover_art: true,
	cache_ttl_audiodb_found: 168,
	cache_ttl_audiodb_not_found: 24,
	cache_ttl_audiodb_library: 336,
	cache_ttl_recently_viewed_bytes: 48
} as unknown as AdvancedSettingsForm;

const TOGGLE_ROWS: Array<{ testId: string; description: string }> = [
	{ testId: 'audiodb-toggle-enabled', description: 'Fetch images from TheAudioDB' },
	{
		testId: 'audiodb-toggle-name-search',
		description: 'Try artist/album name search when MusicBrainz ID lookup returns no images'
	},
	{
		testId: 'audiodb-toggle-direct-remote',
		description: "Load images directly from TheAudioDB's CDN"
	},
	{ testId: 'audiodb-toggle-prefer-local', description: 'Use cover art from your own files first' }
];

describe('SettingsAudioDB toggle layout', () => {
	it('constrains toggle rows so long descriptions wrap instead of overlapping (#442)', async () => {
		await render(SettingsAudioDB, { props: { data } });

		for (const row of TOGGLE_ROWS) {
			const label = page.getByTestId(row.testId);
			await expect.element(label).toHaveClass(/w-full/);
			await expect.element(label).toHaveClass(/justify-between/);
			await expect
				.element(label.getByText(row.description, { exact: true }))
				.toHaveClass(/min-w-0/);
		}

		const toggles = await page.getByRole('checkbox').all();
		expect(toggles).toHaveLength(4);
		for (const toggle of toggles) {
			await expect.element(toggle).toHaveClass(/shrink-0/);
		}
	});
});
