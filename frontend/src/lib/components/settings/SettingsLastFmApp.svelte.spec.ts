import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import { SECRET_MASK } from '$lib/ui/secretMask';

import SettingsLastFmApp from './SettingsLastFmApp.svelte';

describe('SettingsLastFmApp', () => {
	it('tests the saved pair by sending the masks back', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/lastfm': {
				enabled: true,
				api_key: SECRET_MASK,
				shared_secret: SECRET_MASK
			},
			'POST /api/v3/settings/lastfm/verify': {
				valid: false,
				message: 'The shared secret does not match the key.'
			}
		});

		await renderWithQuery(SettingsLastFmApp, {});
		await page.getByRole('button', { name: 'Test connection' }).click();

		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('The shared secret does not match the key.');
		const [sent] = api.calls('POST /api/v3/settings/lastfm/verify');
		expect(sent?.body).toMatchObject({ api_key: SECRET_MASK, shared_secret: SECRET_MASK });
	});

	it('asks for a key before testing a blank one', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/lastfm': { enabled: false, api_key: '', shared_secret: '' }
		});

		await renderWithQuery(SettingsLastFmApp, {});
		await page.getByRole('button', { name: 'Test connection' }).click();

		await expect.element(page.getByRole('status')).toHaveTextContent('Add an API key first.');
		expect(api.calls('POST /api/v3/settings/lastfm/verify')).toHaveLength(0);
	});
});
