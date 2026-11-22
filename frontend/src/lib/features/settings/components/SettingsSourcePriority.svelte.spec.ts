import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';

import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import SettingsSourcePriority from './SettingsSourcePriority.svelte';

describe('the source priority list', () => {
	it('lists Deezer as a source of its own', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/download-clients/source-priority': {
				order: ['deezer', 'soulseek', 'usenet']
			},
			'GET /api/v3/plugins/sources': { sources: [] },
			'GET /api/v3/settings/download-clients/sabnzbd': {
				enabled: false,
				url: '',
				api_key: '',
				category: '',
				downloads_mount: '/sabnzbd-downloads',
				priority: 0
			},
			'GET /api/v3/settings/download-clients/nzbget': {
				enabled: false,
				url: '',
				username: '',
				password: '',
				category: '',
				downloads_mount: '/nzbget-downloads'
			},
			'PUT /api/v3/settings/download-clients/source-priority': ({ body }) =>
				body as { order: string[] }
		});
		await renderWithQuery(SettingsSourcePriority, {});

		await expect.element(page.getByText('Deezer', { exact: true })).toBeVisible();
		await expect.element(page.getByText('· Deemix')).toBeVisible();
		await expect.element(page.getByText('· Removed')).not.toBeInTheDocument();

		await page.getByRole('button', { name: 'Move Deezer down' }).click();
		await expect
			.poll(() => api.calls('PUT /api/v3/settings/download-clients/source-priority').length)
			.toBe(1);
		expect(api.calls('PUT /api/v3/settings/download-clients/source-priority')[0].body).toEqual({
			order: ['soulseek', 'deezer', 'usenet']
		});
	});
});
