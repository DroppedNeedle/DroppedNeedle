import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import SettingsDiagnostics from './SettingsDiagnostics.svelte';

describe('SettingsDiagnostics', () => {
	it('groups the outbound calls by provider', async () => {
		fakeApi({
			'GET /api/v3/admin/queue-stats': { jobs: [], queues: [] },
			'GET /api/v3/admin/provider-stats': {
				limiters: [{ source: 'musicbrainz', per_second: 1, burst: 1, remaining: 1 }],
				calls: [
					{
						source: 'musicbrainz',
						priority: 'background_sync',
						outcome: 'rate_limited',
						calls: 2,
						calls_last_minute: 0,
						body_bytes: 0
					},
					{
						source: 'coverartarchive',
						priority: 'image_fetch',
						outcome: 'ok',
						calls: 1200,
						calls_last_minute: 40,
						body_bytes: 5_242_880
					},
					{
						source: 'musicbrainz',
						priority: 'user_initiated',
						outcome: 'ok',
						calls: 10,
						calls_last_minute: 3,
						body_bytes: 2048
					}
				]
			}
		});

		await renderWithQuery(SettingsDiagnostics, {});

		const table = page.getByRole('region', { name: 'Outbound provider calls' }).getByRole('table');
		const groups = table.getByRole('rowgroup');
		await expect.element(groups.nth(1)).toHaveTextContent('Cover Art Archive 1,200 calls');
		await expect.element(groups.nth(1)).toHaveTextContent(/Artwork\s*OK\s*1,200\s*40\s*5\.0 MB/);
		await expect.element(groups.nth(2)).toHaveTextContent('MusicBrainz 12 calls');
		const musicbrainz = groups.nth(2).getByRole('row');
		await expect.element(musicbrainz.nth(1)).toHaveTextContent(/User\s*OK\s*10\s*3\s*2\.0 KB/);
		await expect
			.element(musicbrainz.nth(2))
			.toHaveTextContent(/Background\s*Rate limited\s*2\s*0\s*0 B/);
	});
});
