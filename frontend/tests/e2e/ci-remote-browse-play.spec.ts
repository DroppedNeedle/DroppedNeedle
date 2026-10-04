import { expect, test } from '@playwright/test';

import { installApiMocks, standardUser, type MockTable } from './mockApi';

/**
 * Remote browse and play: a signed-in user browses their Plex tracks and
 * starts one; the player bar picks it up. The Plex server and the audio
 * bytes are both stubbed. All API answers are stubbed; no live network.
 *
 * Dependency note: if the playback agent changes the player-bar shape
 * (Player.svelte) or the Plex stream handshake, this flow's final assertion
 * follows that restored shape.
 */

const plexTracks = {
	items: [
		{
			plex_id: 'plex-track-1',
			title: 'Copper Static',
			track_number: 1,
			duration_seconds: 187,
			disc_number: 1,
			album_name: 'Copper Static',
			artist_name: 'The Fixtures',
			codec: 'flac',
			bitrate: 900,
			audio_channels: 2,
			container: 'flac',
			part_key: '/library/parts/1/file.flac',
			image_url: null
		},
		{
			plex_id: 'plex-track-2',
			title: 'Tin Echo',
			track_number: 2,
			duration_seconds: 203,
			disc_number: 1,
			album_name: 'Copper Static',
			artist_name: 'The Fixtures',
			codec: 'flac',
			bitrate: 850,
			audio_channels: 2,
			container: 'flac',
			part_key: '/library/parts/2/file.flac',
			image_url: null
		}
	],
	total: 2,
	offset: 0,
	limit: 48
};

test.describe('remote browse and play', () => {
	test('browsing Plex tracks and starting one shows it in the player', async ({ page }) => {
		const extra: MockTable = {
			'GET /api/v1/plex/tracks': plexTracks,
			'GET /api/v1/plex/albums': { items: [], total: 0, offset: 0, limit: 48 }
		};
		await installApiMocks(page, { user: standardUser, extra });

		await page.goto('/library/plex/tracks');

		// The remote catalog lists both tracks.
		await expect(page.getByText('Copper Static').first()).toBeVisible();
		await expect(page.getByText('Tin Echo').first()).toBeVisible();

		// Starting a track hands it to the player bar. (The row's aria-label
		// renders the uninterpolated literal `Play {track.title}` — see the
		// report — so select the row by its title text instead.)
		await page.locator('tbody tr', { hasText: 'Copper Static' }).first().click();
		const playerBar = page.locator('.droppedneedle-player-bar');
		await expect(playerBar).toBeVisible({ timeout: 15_000 });
		await expect(playerBar).toContainText('Copper Static');
	});
});
