import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';

import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type RouteData } from '$lib/test/api';
import { queryClient } from '$lib/queries/QueryClient';
import { renderWithQuery } from '$lib/test/render';

import SettingsSoundAnalysis from './SettingsSoundAnalysis.svelte';

type Status = RouteData<'GET /api/v3/settings/library/sound-analysis/status'>;
type Saved = RouteData<'PUT /api/v3/settings/library/sound-analysis'>;

function signIn() {
	authStore.setUser(
		toAuthUser({
			id: 'admin-1',
			display_name: 'admin',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'admin',
			username_display: 'admin'
		})
	);
}

function status(overrides: Partial<Status> = {}): Status {
	return {
		state: 'off',
		waiting_for: null,
		tracks: 2128,
		analysed: 0,
		to_go: 2128,
		not_analysed: 0,
		library_seconds: 22_557,
		seconds_left: 22_557,
		last_not_analysed: null,
		model: 'MSD_musicnn',
		analysis_version: 1,
		...overrides
	};
}

describe('the sound analysis card on Settings > Library', () => {
	it('is off by default and turns on with one press', async () => {
		signIn();
		let saved: Saved = { sound_analysis_enabled: false, sound_analysis_paused: false };
		const api = fakeApi({
			'GET /api/v3/settings/library/sound-analysis': () => saved,
			'PUT /api/v3/settings/library/sound-analysis': (request) => {
				saved = request.body as Saved;
				return saved;
			},
			'GET /api/v3/settings/library/sound-analysis/status': () =>
				status(saved.sound_analysis_enabled ? { state: 'running' } : {})
		});
		await renderWithQuery(SettingsSoundAnalysis, {}, { client: queryClient });

		const toggle = page.getByRole('checkbox', { name: 'Analyse how tracks sound' });
		await expect.element(toggle).not.toBeChecked();
		await expect
			.element(page.getByText('About 6 hours for this library', { exact: false }))
			.toBeVisible();
		await expect.element(page.getByText('Off')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Pause' })).not.toBeInTheDocument();

		await toggle.click();

		await expect.element(toggle).toBeChecked();
		expect(
			api.calls('PUT /api/v3/settings/library/sound-analysis').map((call) => call.body)
		).toEqual([{ sound_analysis_enabled: true, sound_analysis_paused: false }]);
		await expect.element(page.getByText('Running')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Pause' })).toBeVisible();
	});

	it('shows how far it has come, why it waits and the last file it could not read', async () => {
		signIn();
		let saved: Saved = { sound_analysis_enabled: true, sound_analysis_paused: false };
		const api = fakeApi({
			'GET /api/v3/settings/library/sound-analysis': () => saved,
			'PUT /api/v3/settings/library/sound-analysis': (request) => {
				saved = request.body as Saved;
				return saved;
			},
			'GET /api/v3/settings/library/sound-analysis/status': () =>
				saved.sound_analysis_paused
					? status({ state: 'paused', analysed: 1240, to_go: 876, not_analysed: 12 })
					: status({
							state: 'waiting',
							waiting_for: 'scan',
							analysed: 1240,
							to_go: 876,
							not_analysed: 12,
							seconds_left: 10_800,
							last_not_analysed: {
								relative_path: 'Yes/Close to the Edge/01 Close to the Edge.flac',
								outcome: 'undecodable'
							}
						})
		});
		await renderWithQuery(SettingsSoundAnalysis, {}, { client: queryClient });

		await expect.element(page.getByText('Waiting', { exact: true })).toBeVisible();
		await expect.element(page.getByText('1,240')).toBeVisible();
		await expect.element(page.getByText('876')).toBeVisible();
		await expect.element(page.getByText('~3 h')).toBeVisible();
		await expect
			.element(page.getByText('Waiting for the scan to finish. Carries on by itself.'))
			.toBeVisible();
		await expect
			.element(page.getByText('Yes/Close to the Edge/01 Close to the Edge.flac'))
			.toBeVisible();
		await expect
			.element(page.getByText('The file would not decode. It is tried again if it changes.'))
			.toBeVisible();
		await expect.element(page.getByText('Model MSD_musicnn - analysis version 1')).toBeVisible();

		await page.getByRole('button', { name: 'Pause' }).click();

		await expect.element(page.getByRole('button', { name: 'Resume' })).toBeVisible();
		expect(
			api.calls('PUT /api/v3/settings/library/sound-analysis').map((call) => call.body)
		).toEqual([{ sound_analysis_enabled: true, sound_analysis_paused: true }]);
		await expect
			.element(page.getByRole('checkbox', { name: 'Analyse how tracks sound' }))
			.toBeChecked();
		await expect.element(page.getByText('Paused', { exact: true })).toBeVisible();
	});
});
