import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { SectionPrefItem } from '$lib/types';

vi.mock('$env/dynamic/public', () => ({
	env: { PUBLIC_API_URL: '' }
}));

const { integrationState, prefsState, authState } = vi.hoisted(() => ({
	integrationState: {
		loaded: true,
		youtube: false,
		jellyfin: false,
		navidrome: false,
		plex: false,
		localfiles: false,
		download_client: false
	},
	prefsState: {
		data: undefined as { pages: Record<string, SectionPrefItem[]> } | undefined,
		isLoading: false
	},
	authState: { isAdmin: false }
}));

vi.mock('$lib/stores/integration', () => ({
	integrationStore: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb(integrationState);
			return () => {};
		}),
		ensureLoaded: vi.fn().mockResolvedValue(undefined),
		reset: vi.fn()
	}
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: authState
}));
vi.mock('$lib/stores/nowPlayingMerged.svelte', () => ({
	nowPlayingMerged: { isSourcePlaying: () => false, primarySession: null }
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: { isPlaying: false, nowPlaying: null }
}));
vi.mock('$lib/queries/section-prefs/SectionPrefsQuery.svelte', () => ({
	getSectionPrefsQuery: () => prefsState
}));

import SidebarServices from './SidebarServices.svelte';

function sidebarPrefs(disabledKeys: string[]): { pages: Record<string, SectionPrefItem[]> } {
	const keys = ['youtube', 'jellyfin', 'navidrome', 'plex', 'localfiles'];
	return {
		pages: {
			sidebar: keys.map((key) => ({
				key,
				title: key,
				description: '',
				zone: 'Services',
				enabled: !disabledKeys.includes(key),
				available: true,
				requires: null
			}))
		}
	};
}

describe('SidebarServices', () => {
	beforeEach(() => {
		Object.assign(integrationState, {
			loaded: true,
			youtube: false,
			jellyfin: false,
			navidrome: false,
			plex: false,
			localfiles: false,
			download_client: false
		});
		prefsState.data = undefined;
		prefsState.isLoading = false;
		authState.isAdmin = false;
	});

	it('renders a link for a connected service', async () => {
		integrationState.jellyfin = true;
		prefsState.data = sidebarPrefs([]);
		await render(SidebarServices);

		const link = page.getByRole('link', { name: 'Jellyfin' });
		await expect.element(link).toBeInTheDocument();
	});

	it('hides a connected service the user disabled', async () => {
		integrationState.jellyfin = true;
		integrationState.navidrome = true;
		prefsState.data = sidebarPrefs(['jellyfin']);
		await render(SidebarServices);

		await expect.element(page.getByRole('link', { name: 'Navidrome' })).toBeInTheDocument();
		await expect.element(page.getByText('Jellyfin')).not.toBeInTheDocument();
	});

	it('hides the admin connect hint for a disabled service', async () => {
		authState.isAdmin = true;
		prefsState.data = sidebarPrefs(['youtube']);
		await render(SidebarServices);

		// other hints remain, the hidden one is gone entirely
		await expect.element(page.getByText('Plex')).toBeInTheDocument();
		await expect.element(page.getByText('YouTube')).not.toBeInTheDocument();
	});

	it('fails open while prefs have not loaded', async () => {
		integrationState.plex = true;
		prefsState.data = undefined;
		prefsState.isLoading = true;
		await render(SidebarServices);

		await expect.element(page.getByRole('link', { name: 'Plex' })).toBeInTheDocument();
	});

	it('shows no connect hints to non-admins', async () => {
		prefsState.data = sidebarPrefs([]);
		await render(SidebarServices);

		await expect.element(page.getByText('YouTube')).not.toBeInTheDocument();
		await expect.element(page.getByText('Jellyfin')).not.toBeInTheDocument();
	});
});
