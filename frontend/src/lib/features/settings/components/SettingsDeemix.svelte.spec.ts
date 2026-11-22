import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';

import type { components } from '$lib/api/v3/openapi';
import { queryClient } from '$lib/queries/QueryClient';
import { fakeApi } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

import SettingsDeemix from './SettingsDeemix.svelte';

type Status = components['schemas']['DeemixStatusResponse'];

const saved: components['schemas']['DeemixConnection'] = {
	enabled: true,
	url: 'http://deemix:6595',
	downloads_mount: '/deemix-downloads'
};

const signedIn: Status = {
	enabled: true,
	configured: true,
	reachable: true,
	deezer_reachable: true,
	signed_in: true,
	sign_in_offered: false,
	account_quality: 'flac',
	best_quality: 'flac',
	webui_version: '4.8.0',
	deemix_version: '3.14.1',
	message: 'Connected',
	warnings: []
};

describe('the Deemix card', () => {
	it("shows the connection and the Deezer account's quality", async () => {
		fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': signedIn
		});
		await renderWithQuery(SettingsDeemix, {});

		await expect
			.element(page.getByText('Connected · v4.8.0 · Deezer account: FLAC 16-bit'))
			.toBeVisible();
		await expect.element(page.getByRole('status', { name: 'Connected' })).toBeVisible();
	});

	it('shows what to change in Deemix first', async () => {
		fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': {
				...signedIn,
				warnings: ['overwrite_off', 'fallback_on']
			}
		});
		await renderWithQuery(SettingsDeemix, {});
		await page.getByRole('button', { name: 'Expand' }).click();

		await expect.element(page.getByText('Set Overwrite files to Yes in Deemix.')).toBeVisible();
		await expect
			.element(page.getByText('Deemix may lower quality. Turn off its fallback options.'))
			.not.toBeInTheDocument();
	});

	it("says when the quality settings refuse the account's quality", async () => {
		fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': {
				...signedIn,
				account_quality: 'mp3_128',
				best_quality: null,
				warnings: ['quality_not_taken']
			}
		});
		await renderWithQuery(SettingsDeemix, {});
		await page.getByRole('button', { name: 'Expand' }).click();

		await expect
			.element(
				page.getByText("Your Deezer account gives MP3 128; your quality settings don't allow it.")
			)
			.toBeVisible();
	});

	it("shows no quality warning the server didn't raise", async () => {
		fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': {
				...signedIn,
				account_quality: 'mp3_128',
				best_quality: null
			}
		});
		await renderWithQuery(SettingsDeemix, {});
		await page.getByRole('button', { name: 'Expand' }).click();

		await expect
			.element(page.getByText('Connected · v4.8.0 · Deezer account: MP3 128'))
			.toBeVisible();
		await expect.element(page.getByText(/quality settings don't allow/)).not.toBeInTheDocument();
	});

	it('passes a Test of a signed-out Deemix that offers its saved sign-in', async () => {
		const offered = 'Deemix has a saved Deezer sign-in; DroppedNeedle signs in when it needs to.';
		fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': signedIn,
			'POST /api/v3/settings/download-clients/deemix/test': {
				...signedIn,
				signed_in: false,
				sign_in_offered: true,
				account_quality: null,
				best_quality: null,
				message: offered
			}
		});
		await renderWithQuery(SettingsDeemix, {});
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('button', { name: 'Test connection' }).click();

		await expect
			.element(page.getByRole('status').filter({ hasText: offered }).last())
			.toHaveClass(/text-success/);
	});

	it("drops a Test's result once a save reads the status again", async () => {
		const api = fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': signedIn,
			'PUT /api/v3/settings/download-clients/deemix': saved,
			'POST /api/v3/settings/download-clients/deemix/test': {
				...signedIn,
				warnings: ['overwrite_off']
			}
		});
		// A save's refresh goes to the app's client.
		await renderWithQuery(SettingsDeemix, {}, { client: queryClient });
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('button', { name: 'Test connection' }).click();
		const overwrite = page.getByText('Set Overwrite files to Yes in Deemix.').first();
		await expect.element(overwrite).toBeVisible();

		await page.getByRole('textbox', { name: 'Downloads folder' }).fill('/deemix-elsewhere');
		await page.getByRole('button', { name: 'Save', exact: true }).click();

		await expect.poll(() => api.calls('GET /api/v3/acquire/deemix/status').length).toBe(2);
		await expect.element(overwrite).not.toBeInTheDocument();
	});

	it('tests the values on screen', async () => {
		const api = fakeApi({
			'GET /api/v3/settings/download-clients/deemix': saved,
			'GET /api/v3/acquire/deemix/status': { ...signedIn, reachable: false, signed_in: false },
			'POST /api/v3/settings/download-clients/deemix/test': {
				...signedIn,
				signed_in: false,
				account_quality: null,
				best_quality: null,
				message: 'Save to see your Deezer account.'
			}
		});
		await renderWithQuery(SettingsDeemix, {});
		await page.getByRole('button', { name: 'Expand' }).click();
		await page.getByRole('textbox', { name: 'Deemix URL' }).fill('http://deemix.lan:6595');
		await page.getByRole('button', { name: 'Test connection' }).click();

		await expect.element(page.getByText('Save to see your Deezer account.').first()).toBeVisible();
		expect(
			api.calls('POST /api/v3/settings/download-clients/deemix/test').map((call) => call.body)
		).toEqual([{ ...saved, url: 'http://deemix.lan:6595' }]);
	});
});
