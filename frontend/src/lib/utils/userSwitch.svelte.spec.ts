import { entries } from 'idb-keyval';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

import { PAGE_SOURCE_KEYS } from '$lib/constants';
import { queryClient } from '$lib/queries/QueryClient';
import {
	getMusicBrainzSourceScope,
	setMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import { getScrobblePreferencesQueryOptions } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte';
import { authStore, LAST_USER_ID_KEY, type AuthUser } from '$lib/stores/authStore.svelte';
import '$lib/stores/musicSource';
import { playerStore } from '$lib/stores/player.svelte';
import '$lib/stores/scrobble.svelte';
import { fakeApi } from '$lib/test/api';
import { logout } from '$lib/utils/logout';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';

// Kept per device, not per person: how loud, how it sounds, how it looks.
const DEVICE_KEYS = [
	'droppedneedle_player_volume',
	'droppedneedle_eq_settings',
	'droppedneedle_theme',
	'droppedneedle_sampler_volume'
];

function user(id: string, name: string): AuthUser {
	return {
		id,
		display_name: name,
		role: 'user',
		email: null,
		avatar_url: null,
		username: id,
		username_display: name,
		providers: ['local']
	};
}

const ALICE = user('user-a', 'Alice');
const BOB = user('user-b', 'Bob');

/** Alice's session as the app leaves it: a cached and persisted query, her stores and storage. */
async function signInAlice(): Promise<void> {
	authStore.setUser(ALICE);
	localStorage.setItem(LAST_USER_ID_KEY, ALICE.id);
	fakeApi({
		'GET /api/v3/me/scrobble-preferences': { primary_music_source: 'lastfm' }
	});
	await queryClient.fetchQuery(getScrobblePreferencesQueryOptions(ALICE.id));
	await expect.poll(async () => (await entries()).length).toBeGreaterThan(0);

	setMusicBrainzSourceScope({ source_mode: 'mirror', source_id: 'mirror-a', generation: 7 });
	localStorage.setItem(PAGE_SOURCE_KEYS.home, JSON.stringify('lastfm'));
	localStorage.setItem(
		'droppedneedle_player_session',
		JSON.stringify({
			nowPlaying: { trackName: "Alice's song", artistName: 'Alice', sourceType: 'local' },
			queue: [],
			currentIndex: 0,
			progress: 12,
			shuffleEnabled: false,
			shuffleOrder: []
		})
	);
	for (const key of DEVICE_KEYS) localStorage.setItem(key, 'kept');
}

async function expectNothingOfAlice(): Promise<void> {
	expect(queryClient.getQueryCache().getAll()).toEqual([]);
	expect(await entries()).toEqual([]);
	expect(Object.keys(localStorage).sort()).toEqual([...DEVICE_KEYS].sort());
	expect(JSON.stringify({ ...localStorage })).not.toContain(ALICE.id);
	expect(playerStore.restoreSession()).toBeNull();
	expect(getMusicBrainzSourceScope().userId).not.toBe(ALICE.id);
	expect(authStore.user?.id).not.toBe(ALICE.id);
}

beforeEach(async () => {
	localStorage.clear();
	await clearUserSessionState();
});

describe('signing in as another person on the same browser', () => {
	it('leaves nothing of the first person after a sign-out', async () => {
		await signInAlice();
		fakeApi({ 'POST /api/v3/auth/logout': undefined });

		await logout();
		authStore.setUser(BOB);

		await expectNothingOfAlice();
	});

	it('leaves nothing of the first person when the next session arrives without a sign-out', async () => {
		await signInAlice();

		// The start-up check: a different account than last time clears the old one.
		authStore.setUser(BOB);
		await clearUserSessionState({ clearAuth: false });

		await expectNothingOfAlice();
		expect(authStore.user?.id).toBe(BOB.id);
	});
});
