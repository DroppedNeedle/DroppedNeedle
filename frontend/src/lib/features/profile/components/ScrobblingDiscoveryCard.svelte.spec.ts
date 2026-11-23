import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { apiError, fakeApi, type FakeRequest } from '$lib/test/api';
import { queryClient } from '$lib/queries/QueryClient';
import { renderWithQuery } from '$lib/test/render';
import type { ScrobblePreferences } from '$lib/features/profile/queries/scrobble-preferences.svelte';
import type { IntegrationStatus } from '$lib/features/home/queries/home.svelte';

// The preferences read waits for a signed-in user.
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: { user: { id: 'u1', providers: ['oidc'] }, isAuthenticated: true }
}));

import ScrobblingDiscoveryCard from './ScrobblingDiscoveryCard.svelte';

const prefs: ScrobblePreferences = {
	scrobble_to_lastfm: false,
	scrobble_to_listenbrainz: false,
	navidrome_handles_external_scrobbles: true,
	primary_music_source: 'listenbrainz',
	now_playing_visibility: 'full',
	auto_request_personal_mix: false
};

const integrations = (lastfm: boolean): IntegrationStatus => ({
	download_client: false,
	jellyfin: false,
	lastfm,
	library: false,
	listenbrainz: false,
	localfiles: false,
	navidrome: false,
	plex: false,
	youtube: false,
	youtube_api: false
});

afterEach(() => {
	vi.restoreAllMocks();
	// The switch is read through the app's own query client.
	queryClient.clear();
});

describe('Last.fm link on the profile', () => {
	it('finishes a sign-in that saved the user’s own API key pair first', async () => {
		const open = vi.spyOn(window, 'open').mockReturnValue(null);
		const exchanged: unknown[] = [];
		fakeApi({
			// The own key pair is stored as the Last.fm connection, without an
			// account name until the session exists.
			'GET /api/v3/me/connections': {
				connections: [{ service: 'lastfm', enabled: true, username: '' }]
			},
			'GET /api/v3/me/scrobble-preferences': prefs,
			'GET /api/v3/home/integration-status': integrations(true),
			'POST /api/v3/me/connections/lastfm/token': {
				token: 'lfm-token',
				auth_url: 'https://www.last.fm/api/auth/?api_key=k&token=lfm-token'
			},
			'POST /api/v3/me/connections/lastfm/session': (request: FakeRequest) => {
				exchanged.push(request.body);
				return { username: 'jane', linked: true };
			}
		});
		await renderWithQuery(ScrobblingDiscoveryCard, { navidromeEnabled: false });

		// ListenBrainz comes first and is not linked either: Last.fm's Connect
		// is the last one.
		const connectLastFm = page.getByRole('button', { name: 'Connect' }).last();
		await expect.element(connectLastFm).toBeVisible();

		await connectLastFm.click();
		expect(open).toHaveBeenCalledWith(
			'https://www.last.fm/api/auth/?api_key=k&token=lfm-token',
			'_blank',
			'popup=yes,noopener,noreferrer'
		);
		await page.getByRole('button', { name: 'Finish' }).click();

		// The connections refresh runs through the app's own query client;
		// here the exchange itself is the proof.
		await expect.poll(() => exchanged).toEqual([{ token: 'lfm-token' }]);
		await expect.element(page.getByRole('button', { name: 'Finish' })).not.toBeInTheDocument();
	});

	it('says Last.fm is off instead of offering Connect while the admin left it off', async () => {
		fakeApi({
			'GET /api/v3/me/connections': { connections: [] },
			'GET /api/v3/me/scrobble-preferences': { ...prefs, primary_music_source: 'lastfm' },
			'GET /api/v3/home/integration-status': integrations(false)
		});
		await renderWithQuery(ScrobblingDiscoveryCard, { navidromeEnabled: false });

		await expect.element(page.getByText('Last.fm is off on this server.')).toBeVisible();
		// Only ListenBrainz's Connect is left.
		await expect.element(page.getByRole('button', { name: 'Connect' })).toBeVisible();
		expect(page.getByRole('button', { name: 'Connect' }).elements()).toHaveLength(1);

		// The rows below follow the switch: no scrobbling, and Home and
		// Discover follow ListenBrainz.
		await expect.element(page.getByText('Off on this server.', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Last.fm', exact: true })).toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'ListenBrainz', exact: true }))
			.toHaveAttribute('aria-pressed', 'true');
	});

	it('asks for the user’s own key pair when no key pair is set up', async () => {
		fakeApi({
			'GET /api/v3/me/connections': { connections: [] },
			'GET /api/v3/me/scrobble-preferences': prefs,
			'GET /api/v3/home/integration-status': integrations(true),
			'POST /api/v3/me/connections/lastfm/token': apiError(
				409,
				'PROVIDER_NOT_CONFIGURED',
				'Last.fm is not set up yet.',
				{ provider: 'lastfm' }
			)
		});
		await renderWithQuery(ScrobblingDiscoveryCard, { navidromeEnabled: false });

		const connectLastFm = page.getByRole('button', { name: 'Connect' }).last();
		await expect.element(connectLastFm).toBeVisible();
		await connectLastFm.click();
		await expect.element(page.getByLabelText('Last.fm API key')).toBeVisible();
	});
});

describe('Your history on the profile', () => {
	it('says what is kept and forgets it once confirmed', async () => {
		const api = fakeApi({
			'GET /api/v3/me/connections': { connections: [] },
			'GET /api/v3/me/scrobble-preferences': prefs,
			'GET /api/v3/home/integration-status': integrations(true),
			'DELETE /api/v3/discover/history': undefined
		});
		const forgotten = () => api.calls('DELETE /api/v3/discover/history').length;
		await renderWithQuery(ScrobblingDiscoveryCard, { navidromeEnabled: false });

		await expect
			.element(
				page.getByText(
					'Kept for you: your plays here, what you told Discover, the picks it showed you',
					{ exact: false }
				)
			)
			.toBeVisible();
		await page.getByRole('button', { name: 'Forget my history' }).click();
		await expect
			.element(page.getByText('This removes the picks and the play endings.', { exact: false }))
			.toBeVisible();
		expect(forgotten()).toBe(0);

		await page.getByRole('button', { name: 'Forget', exact: true }).click();
		await expect.poll(forgotten).toBe(1);
		await expect
			.element(page.getByRole('button', { name: 'Forget', exact: true }))
			.not.toBeInTheDocument();
	});

	it.each([
		[500, 'INTERNAL_ERROR', 'Not all of it was removed. Forget again to finish.'],
		[429, 'RATE_LIMITED', 'Too many tries just now. Try again in a minute.']
	])('says what to do when the forget answers %i', async (status, code, line) => {
		fakeApi({
			'GET /api/v3/me/connections': { connections: [] },
			'GET /api/v3/me/scrobble-preferences': prefs,
			'GET /api/v3/home/integration-status': integrations(true),
			'DELETE /api/v3/discover/history': apiError(status, code)
		});
		await renderWithQuery(ScrobblingDiscoveryCard, { navidromeEnabled: false });

		await page.getByRole('button', { name: 'Forget my history' }).click();
		await page.getByRole('button', { name: 'Forget', exact: true }).click();
		await expect.element(page.getByText(line)).toBeVisible();
	});
});
