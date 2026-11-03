import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';

import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import SettingsPreferences from './SettingsPreferences.svelte';

describe('Release Types', () => {
	it('keeps an unsaved edit when the saved choices are read again', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/preferences': {
				primary_types: ['album'],
				secondary_types: ['studio']
			}
		});
		const { client } = await renderWithQuery(SettingsPreferences, {});
		const ep = page.getByRole('checkbox', { name: 'Show EP' });
		const album = page.getByRole('checkbox', { name: 'Show Album' });
		await expect.element(album).toBeChecked();

		await ep.click();
		await expect.element(ep).toBeChecked();

		await client.refetchQueries();
		expect(api.calls('GET /api/v3/settings/preferences')).toHaveLength(2);
		await expect.element(ep).toBeChecked();
	});
});
