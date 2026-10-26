import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';

// The page reads the profile, the three remote connections and the library
// stats; stub those reads and every card so the test drives only the
// Navidrome connection.
const { state, stub } = vi.hoisted(() => ({
	state: { navidromeConnected: false },
	stub: () => {
		const Comp = function () {};
		Comp.prototype = {};
		return { default: Comp };
	}
}));

vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/profile') }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'u1', providers: ['oidc'] } }
}));
vi.mock('$lib/queries/profile/ProfileQuery.svelte', () => ({
	getProfileQuery: () => ({
		data: { id: 'u1', providers: ['oidc'] },
		isPending: false,
		isError: false,
		refetch: vi.fn()
	})
}));
vi.mock('$lib/queries/remotes/RemoteQueries.svelte', () => ({
	getRemoteConnectionQuery: (source: () => string) => ({
		isPending: false,
		get data() {
			return {
				connected: source() === 'navidrome' && state.navidromeConnected,
				account_label: ''
			};
		}
	}),
	getRemoteStatsQuery: () => ({ isPending: false, data: undefined })
}));
vi.mock('$lib/queries/library/LibraryQueries.svelte', () => ({
	getLibraryStatsQuery: () => ({ data: undefined })
}));
vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: vi.fn() }));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));
vi.mock('$lib/utils/logout', () => ({ logout: vi.fn() }));

vi.mock('$lib/components/profile/AccountCard.svelte', stub);
vi.mock('$lib/components/profile/ConnectedServicesGrid.svelte', stub);
vi.mock('$lib/components/profile/LibraryStatsList.svelte', stub);
vi.mock('$lib/components/profile/MediaServerAccountsCard.svelte', stub);
vi.mock('$lib/components/profile/NavidromeMusicFoldersCard.svelte', stub);
vi.mock('$lib/components/profile/ProfileHeader.svelte', stub);
vi.mock('$lib/components/profile/SessionsManager.svelte', stub);
vi.mock('$lib/components/profile/AppearanceCard.svelte', stub);
vi.mock('$lib/components/profile/ScrobblingDiscoveryCard.svelte', stub);
vi.mock('$lib/components/profile/SpotifyConnectionCard.svelte', stub);
vi.mock('$lib/components/profile/ProfileConnectApps.svelte', stub);
vi.mock('$lib/components/PageSectionToc.svelte', stub);

import ProfilePage from './+page.svelte';

describe('profile page music folders', () => {
	beforeEach(() => {
		state.navidromeConnected = false;
	});

	it('leaves the Navidrome music folders out while Navidrome is not connected', () => {
		render(ProfilePage);
		expect(document.getElementById('scrobbling')).not.toBeNull();
		expect(document.getElementById('navidrome-music-folders')).toBeNull();
	});

	it('shows the Navidrome music folders once Navidrome is connected', () => {
		state.navidromeConnected = true;
		render(ProfilePage);
		expect(document.getElementById('navidrome-music-folders')).not.toBeNull();
	});
});
