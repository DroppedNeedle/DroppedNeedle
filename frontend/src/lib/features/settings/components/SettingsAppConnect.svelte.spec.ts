import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { fakeApi, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import SettingsAppConnect from './SettingsAppConnect.svelte';

type Settings = RouteData<'GET /api/v3/settings/app-connect'>;
const preset: Settings['clients'][number] = {
	client_id: 'otohari',
	name: 'OtoHari',
	credential_kind: 'device_session',
	redirect_uris: ['app.droppedneedle.mobile://connect']
};

describe('app registrations', () => {
	it('enables the preset only on request, alongside existing app-password registrations', async () => {
		const existing = { client_id: 'player', name: 'Player', redirect_uris: ['org.example:/auth'] };
		const api = fakeApi({
			'GET /api/v3/settings/app-connect': { clients: [existing] },
			'PUT /api/v3/settings/app-connect': {
				clients: [{ ...existing, credential_kind: 'app_password' }, preset]
			}
		});
		await renderWithQuery(SettingsAppConnect, {});

		await expect.element(page.getByRole('button', { name: 'Enable OtoHari' })).toBeVisible();
		expect(api.calls('PUT /api/v3/settings/app-connect')).toHaveLength(0);
		await page.getByRole('button', { name: 'Enable OtoHari' }).click();
		await expect
			.poll(() => api.calls('PUT /api/v3/settings/app-connect')[0]?.body)
			.toEqual({
				clients: [{ ...existing, credential_kind: 'app_password' }, preset]
			});
		await expect
			.element(page.getByRole('button', { name: 'Enable OtoHari' }))
			.not.toBeInTheDocument();
	});

	it('offers either access type for a custom app and keeps unsaved edits in the draft', async () => {
		const api = fakeApi({ 'GET /api/v3/settings/app-connect': { clients: [] } });
		await renderWithQuery(SettingsAppConnect, {});

		await page.getByRole('button', { name: 'Add app', exact: true }).click();
		const access = page.getByRole('combobox', { name: 'Access type' });
		await expect.element(access).toHaveValue('app_password');
		await access.selectOptions('device_session');
		await expect.element(page.getByText(/including administrator actions/).first()).toBeVisible();
		await page.getByRole('button', { name: 'Add OtoHari' }).click();
		await expect.element(page.getByRole('button', { name: 'Remove OtoHari' })).toBeVisible();
		expect(api.calls('PUT /api/v3/settings/app-connect')).toHaveLength(0);
		await page.getByRole('button', { name: 'Discard' }).click();
		await expect.element(page.getByRole('button', { name: 'Enable OtoHari' })).toBeVisible();
	});
});
