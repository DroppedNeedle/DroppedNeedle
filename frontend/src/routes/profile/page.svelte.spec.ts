import { page } from '@vitest/browser/context';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';

// Shared mock data/helpers live in vi.hoisted so the (hoisted) vi.mock calls can use them.
const { profile, mutationStub, emptyComponent } = vi.hoisted(() => ({
	// Minimal profile payload so the {#if profile} body (which holds the Connect Apps
	// section + hash-scroll target) renders.
	profile: {
		display_name: 'Alice',
		avatar_url: null,
		username: 'alice',
		username_display: 'alice',
		email: null,
		providers: ['local'],
		services: [],
		library_stats: []
	},
	mutationStub: () => ({ mutateAsync: vi.fn(), isPending: false }),
	emptyComponent: () => {
		const C = function () {};
		C.prototype = {};
		return { default: C };
	}
}));

vi.mock('$lib/queries/profile/ProfileQuery.svelte', () => ({
	getProfileQuery: () => ({ data: profile, isPending: false, isError: false, refetch: vi.fn() })
}));
vi.mock('$lib/queries/profile/ProfileMutations.svelte', () => ({
	createUpdateDisplayNameMutation: mutationStub,
	createUpdateUsernameMutation: mutationStub,
	createUpdateEmailMutation: mutationStub,
	createChangePasswordMutation: mutationStub,
	createSetPasswordMutation: mutationStub,
	createUploadAvatarMutation: mutationStub
}));

// Sibling profile cards pull their own query graphs; stub them so the page renders in
// isolation. ProfileConnectApps is left REAL (its queries are mocked below) so the
// page↔section wiring is actually exercised.
vi.mock('$lib/components/profile/MediaServerAccountsCard.svelte', emptyComponent);
vi.mock('$lib/components/profile/NavidromeMusicFoldersCard.svelte', emptyComponent);
vi.mock('$lib/components/profile/ScrobblingDiscoveryCard.svelte', emptyComponent);
vi.mock('$lib/components/profile/SpotifyConnectionCard.svelte', emptyComponent);

vi.mock('$lib/queries/connect-apps/ConnectAppsQueries.svelte', () => ({
	getConnectAppsSettingsQuery: () => ({
		data: { subsonic_enabled: true, jellyfin_enabled: true },
		isLoading: false,
		isError: false
	}),
	getAppPasswordsQuery: () => ({
		data: { items: [], cap: 25, active_count: 0 },
		isLoading: false,
		isError: false
	})
}));
vi.mock('$lib/queries/connect-apps/ConnectAppsMutations.svelte', () => ({
	createAppPassword: mutationStub,
	revokeAppPassword: mutationStub
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		isAdmin: false,
		user: { id: 'user-1', role: 'user', username: 'alice', providers: ['local'] }
	}
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { isPlayerVisible: false }
}));
vi.mock('$lib/api/client', () => ({ ApiError: class ApiError extends Error {} }));
vi.mock('$lib/utils/logout', () => ({ logout: vi.fn() }));
vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: vi.fn() }));
vi.mock('$lib/stores/toast', () => ({ toastStore: { show: vi.fn() } }));
vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$app/state', () => ({ page: { url: new URL('http://localhost/profile#connect-apps') } }));

import ProfilePage from './+page.svelte';

let scrollSpy: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
	scrollSpy = vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(() => {});
});
afterEach(() => scrollSpy.mockRestore());

describe('profile route page', () => {
	it('wires the Connect Apps section into the profile', async () => {
		render(ProfilePage);
		await expect
			.element(page.getByRole('heading', { name: 'Connect Apps', level: 2 }))
			.toBeInTheDocument();
	});

	it('lists the visible profile sections in the page navigation', async () => {
		render(ProfilePage);
		const navigation = page.getByRole('navigation', { name: 'Page sections' });
		await expect.element(navigation.getByRole('link', { name: 'Account' })).toBeInTheDocument();
		await expect
			.element(navigation.getByRole('link', { name: 'Connected Services' }))
			.toBeInTheDocument();
		await expect
			.element(navigation.getByRole('link', { name: 'Connect Apps' }))
			.toBeInTheDocument();
		await expect.element(navigation.getByRole('link', { name: 'Scrobbling' })).toBeInTheDocument();
		await expect.element(navigation.getByRole('link', { name: 'Spotify' })).toBeInTheDocument();
	});

	it('scrolls to the #connect-apps anchor on a cold deep-link once profile has rendered', async () => {
		render(ProfilePage);
		// the effect fires after profile resolves + one animation frame
		await vi.waitFor(() => expect(scrollSpy).toHaveBeenCalled());
		expect(document.getElementById('connect-apps')).not.toBeNull();
	});
});
