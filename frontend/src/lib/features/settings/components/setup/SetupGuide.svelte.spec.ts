import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { toAuthUser } from '$lib/features/auth/queries/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { fakeApi, type FakeRoutes, type RouteData } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';
import SetupGuide from './SetupGuide.svelte';

const SLSKD = 'http://slskd:5030';

const found: RouteData<'GET /api/v3/setup/found'> = {
	services: [
		{ kind: 'slskd', url: SLSKD, outcome: 'found', version: null, note: null },
		{
			kind: 'sabnzbd',
			url: 'http://sabnzbd:8080',
			outcome: 'not_found',
			version: null,
			note: null
		}
	],
	folders: [
		{
			path: '/data/music',
			role: 'library',
			audio_files: 2093,
			counted_all: true,
			writable: true,
			already_set: false
		}
	],
	folder_look: 'complete'
};

const slskd: RouteData<'GET /api/v3/settings/download-client/config'> = {
	enabled: false,
	url: '',
	api_key: '',
	downloads_subpath: '',
	slskd_incomplete_mount: ''
};

const library: RouteData<'GET /api/v3/settings/library'> = {
	library_roots: [],
	staging_path: '',
	naming_script: '',
	acoustid_api_key: '',
	enabled: true,
	actions_applied: [],
	affected_scope_ids: [],
	policy_revision: 'r1',
	reconciliation_required: false,
	reconciliation_state: 'none',
	warnings: []
};

/** A fresh install: nothing saved yet, slskd answering on the network. */
function routes(saved = slskd): FakeRoutes {
	return {
		'GET /api/v3/setup/found': found,
		'GET /api/v3/settings/library': library,
		'GET /api/v3/settings/library/import-folder': { path: '' },
		'GET /api/v3/settings/download-client/config': saved,
		'GET /api/v3/acquire/slskd/status': {
			configured: false,
			reachable: false,
			message: 'Not configured'
		},
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
		'GET /api/v3/settings/download-clients/deemix': {
			enabled: false,
			url: '',
			downloads_mount: '/deemix-downloads'
		},
		'GET /api/v3/settings/indexers': [],
		'GET /api/v3/settings/indexers/search-backend': { backend: 'indexers' },
		'GET /api/v3/settings/prowlarr/config': { enabled: false, url: '', api_key: '' },
		'PUT /api/v3/settings/download-client/config': ({ body }) =>
			body as RouteData<'PUT /api/v3/settings/download-client/config'>
	};
}

describe('the setup guide', () => {
	beforeEach(() => {
		authStore.setUser(
			toAuthUser({
				id: 'admin-1',
				display_name: 'Ada',
				role: 'admin',
				email: null,
				avatar_url: null,
				username: 'ada',
				username_display: 'Ada'
			})
		);
	});

	afterEach(() => authStore.clear());

	it('fills in the slskd form it found and saves only on Save', async () => {
		const api = fakeApi(routes());
		await renderWithQuery(SetupGuide, {});

		await page.getByRole('tab', { name: 'Downloads' }).click();
		await expect.element(page.getByText(SLSKD)).toBeVisible();
		await page.getByRole('button', { name: 'Set up' }).click();

		await expect.element(page.getByLabelText('slskd URL')).toHaveValue(SLSKD);
		await page.getByRole('textbox', { name: 'API key' }).fill('typed-key');
		expect(api.calls('PUT /api/v3/settings/download-client/config')).toHaveLength(0);

		await page.getByRole('button', { name: 'Save', exact: true }).click();
		await expect
			.poll(() => api.calls('PUT /api/v3/settings/download-client/config').length)
			.toBe(1);
		expect(api.calls('PUT /api/v3/settings/download-client/config')[0].body).toMatchObject({
			url: SLSKD,
			api_key: 'typed-key',
			enabled: true
		});
	});

	it('sends nothing when the suggestion is skipped', async () => {
		const api = fakeApi(routes());
		await renderWithQuery(SetupGuide, {});

		await page.getByRole('tab', { name: 'Downloads' }).click();
		await page.getByRole('button', { name: 'Set up' }).click();
		await expect.element(page.getByLabelText('slskd URL')).toHaveValue(SLSKD);
		await page.getByRole('button', { name: 'Skip' }).click();

		await expect.element(page.getByLabelText('slskd URL')).not.toBeInTheDocument();
		await expect.element(page.getByRole('button', { name: 'Set up' })).toBeVisible();
		expect(api.calls('PUT /api/v3/settings/download-client/config')).toHaveLength(0);
	});

	it('says a download client is set up when none answered under its usual name', async () => {
		fakeApi({
			...routes({ ...slskd, enabled: true, url: 'http://soulseek.lan:5030', api_key: '****' }),
			'GET /api/v3/setup/found': {
				...found,
				services: found.services.map((service) => ({ ...service, outcome: 'not_found' }))
			}
		});
		await renderWithQuery(SetupGuide, {});

		await page.getByRole('tab', { name: 'Downloads' }).click();
		await expect.element(page.getByText('slskd is set up.')).toBeVisible();
		await expect.element(page.getByText('Nothing else answered on this network.')).toBeVisible();
		await expect.element(page.getByText(/No download client answered/)).not.toBeInTheDocument();
		await expect.element(page.getByText('Not found: SABnzbd.')).toBeVisible();
	});

	it('lists what is set up and what was skipped at the end', async () => {
		fakeApi(routes({ ...slskd, enabled: true, url: SLSKD, api_key: '********' }));
		await renderWithQuery(SetupGuide, {});

		await page.getByRole('tab', { name: 'Finish' }).click();

		const done = page.getByRole('region', { name: 'Set up' });
		await expect.element(done.getByText('slskd')).toBeVisible();
		const skipped = page.getByRole('region', { name: 'Skipped' });
		await expect.element(skipped.getByText('Library folder')).toBeVisible();
		await expect.element(skipped.getByText('Import folder')).toBeVisible();
		await expect.element(skipped.getByText('A download source')).not.toBeInTheDocument();
	});
});
