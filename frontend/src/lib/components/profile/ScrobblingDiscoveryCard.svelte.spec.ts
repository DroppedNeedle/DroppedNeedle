import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { fakeApi, type FakeRequest } from '$lib/test/api';
import { renderWithQuery } from '$lib/test/render';

// The preferences read waits for a signed-in user.
vi.mock('$lib/stores/authStore.svelte', () => ({
	LAST_USER_ID_KEY: 'test:last-user',
	authStore: { user: { id: 'u1', providers: ['oidc'] }, isAuthenticated: true }
}));

import ScrobblingDiscoveryCard from './ScrobblingDiscoveryCard.svelte';

const prefs = {
	scrobble_to_lastfm: false,
	scrobble_to_listenbrainz: false,
	navidrome_handles_external_scrobbles: true,
	primary_music_source: 'listenbrainz',
	now_playing_visibility: 'full',
	auto_request_personal_mix: false
};

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
		expect(page.getByRole('button', { name: 'Disconnect' }).elements()).toHaveLength(0);

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

	it('shows a linked account as connected', async () => {
		fakeApi({
			'GET /api/v3/me/connections': {
				connections: [{ service: 'lastfm', enabled: true, username: 'jane' }]
			},
			'GET /api/v3/me/scrobble-preferences': prefs
		});
		await renderWithQuery(ScrobblingDiscoveryCard, { navidromeEnabled: false });

		await expect.element(page.getByRole('button', { name: 'Disconnect' })).toBeVisible();
		await expect.element(page.getByText('jane')).toBeVisible();
	});
});
