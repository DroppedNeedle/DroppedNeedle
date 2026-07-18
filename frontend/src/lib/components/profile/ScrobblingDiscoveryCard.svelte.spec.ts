import { page } from '@vitest/browser/context';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

import type { ScrobblePreferences } from '$lib/queries/scrobble-preferences/types';

const h = vi.hoisted(() => ({
	prefs: {
		scrobble_to_lastfm: false,
		scrobble_to_listenbrainz: false,
		navidrome_handles_external_scrobbles: true,
		primary_music_source: 'listenbrainz',
		now_playing_visibility: 'full',
		auto_request_personal_mix: false,
		auto_request_state: 'none'
	} as ScrobblePreferences,
	updatePrefs: vi.fn().mockResolvedValue({}),
	refreshScrobbleSettings: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$lib/api/client', () => ({
	ApiError: class extends Error {}
}));

vi.mock('$lib/queries/connections/ConnectionsQuery.svelte', () => ({
	getConnectionsQuery: () => ({ data: { connections: [] }, isPending: false })
}));

vi.mock('$lib/queries/connections/ConnectionsMutations.svelte', () => ({
	createConnectListenBrainzMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createDisconnectMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLastFmExchangeSessionMutation: () => ({ mutateAsync: vi.fn(), isPending: false }),
	createLastFmRequestTokenMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte', () => ({
	getScrobblePreferencesQuery: () => ({
		get data() {
			return h.prefs;
		},
		isPending: false
	})
}));

vi.mock('$lib/queries/scrobble-preferences/ScrobblePreferencesMutations.svelte', () => ({
	createUpdateScrobblePreferencesMutation: () => ({
		mutateAsync: h.updatePrefs,
		isPending: false
	}),
	createRefreshPersonalMixMutation: () => ({ mutateAsync: vi.fn(), isPending: false })
}));

vi.mock('$lib/stores/scrobble.svelte', () => ({
	scrobbleManager: { refreshSettings: h.refreshScrobbleSettings }
}));

import ScrobblingDiscoveryCard from './ScrobblingDiscoveryCard.svelte';

beforeEach(() => {
	h.prefs = {
		scrobble_to_lastfm: false,
		scrobble_to_listenbrainz: false,
		navidrome_handles_external_scrobbles: true,
		primary_music_source: 'listenbrainz',
		now_playing_visibility: 'full',
		auto_request_personal_mix: false,
		auto_request_state: 'none'
	};
	vi.clearAllMocks();
});

describe('ScrobblingDiscoveryCard', () => {
	it('hides the Navidrome ownership control when Navidrome is unavailable', async () => {
		render(ScrobblingDiscoveryCard, { navidromeEnabled: false });
		expect(
			page.getByRole('checkbox', { name: /Let Navidrome handle Last.fm/ }).elements()
		).toHaveLength(0);
	});

	it('defaults to Navidrome ownership and allows opting out', async () => {
		render(ScrobblingDiscoveryCard, { navidromeEnabled: true });
		const toggle = page.getByRole('checkbox', {
			name: /Let Navidrome handle Last.fm/
		});
		await expect.element(toggle).toBeChecked();
		await expect
			.element(page.getByText(/first disable external scrobbles for the DroppedNeedle player/))
			.toBeInTheDocument();
		await toggle.click();
		expect(h.updatePrefs).toHaveBeenCalledWith({
			navidrome_handles_external_scrobbles: false
		});
		expect(h.refreshScrobbleSettings).toHaveBeenCalled();
	});
});
