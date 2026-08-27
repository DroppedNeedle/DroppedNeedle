import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { createRawSnippet } from 'svelte';

const { routeState, shellModuleState } = vi.hoisted(() => ({
	routeState: { pathname: '/' },
	shellModuleState: {
		playerImports: 0,
		shellFailures: 0,
		playlistFailures: 0,
		discographyFailures: 0
	}
}));
const { batchRequestMock } = vi.hoisted(() => ({ batchRequestMock: vi.fn() }));

vi.mock('$env/dynamic/public', () => ({
	env: {
		PUBLIC_API_URL: ''
	}
}));
vi.mock('$app/environment', () => ({ browser: true, building: false, dev: false }));
vi.mock('$app/navigation', () => ({
	goto: vi.fn(),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn()
}));
vi.mock('$app/paths', () => ({
	base: '',
	assets: '',
	resolve: vi.fn((_route: string, params: Record<string, string>) => `/${params?.id ?? ''}`),
	resolveRoute: vi.fn((_route: string, params: Record<string, string>) => `/${params?.id ?? ''}`),
	asset: vi.fn((file: string) => file)
}));
vi.mock('$app/state', () => ({
	page: {
		get url() {
			return new URL(routeState.pathname, 'http://localhost');
		}
	}
}));
vi.mock('$app/stores', () => ({
	page: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb({
				url: new URL('http://localhost/'),
				params: {},
				route: { id: '/' },
				status: 200,
				error: null,
				data: {},
				form: null,
				state: {}
			});
			return () => {};
		})
	}
}));
vi.mock('$lib/stores/errorModal', () => ({
	errorModal: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb({ show: false });
			return () => {};
		})
	}
}));
vi.mock('$lib/stores/library', () => ({
	libraryStore: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb({
				mbidSet: new Set<string>(),
				requestedSet: new Set<string>(),
				loading: false,
				lastUpdated: null,
				initialized: true
			});
			return () => {};
		}),
		initialize: vi.fn(),
		setSession: vi.fn()
	}
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
vi.mock('$lib/stores/nowPlayingSessions.svelte', () => ({
	nowPlayingStore: { sessions: [], start: vi.fn(), stop: vi.fn() }
}));
vi.mock('$lib/stores/nowPlayingReporter.svelte', () => ({
	nowPlayingReporter: { start: vi.fn(), stop: vi.fn() }
}));
const { followingEventsMock } = vi.hoisted(() => ({
	followingEventsMock: { start: vi.fn(), stop: vi.fn() }
}));
vi.mock('$lib/queries/following/FollowingEvents', () => ({
	createFollowingEvents: vi.fn(() => followingEventsMock)
}));
vi.mock('$lib/stores/cacheTtl.svelte', () => ({ initCacheTTLs: vi.fn() }));
const { syncStatusMock } = vi.hoisted(() => ({
	syncStatusMock: { connect: vi.fn(), disconnect: vi.fn() }
}));
vi.mock('$lib/stores/syncStatus.svelte', () => ({ syncStatus: syncStatusMock }));
vi.mock('$lib/stores/imageSettings', () => ({
	imageSettingsStore: { load: vi.fn().mockResolvedValue(undefined) }
}));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		isPlayerVisible: false,
		isPlaying: false,
		nowPlaying: null,
		progress: 0,
		duration: 0,
		volume: 50,
		currentQueueItem: null,
		togglePlay: vi.fn(),
		seekTo: vi.fn(),
		setVolume: vi.fn(),
		restoreSession: vi.fn(() => null)
	}
}));
vi.mock('$lib/player/launchYouTubePlayback', () => ({ launchYouTubePlayback: vi.fn() }));
vi.mock('$lib/stores/playbackToast.svelte', () => ({
	playbackToast: { visible: false, message: '', type: 'info', show: vi.fn(), dismiss: vi.fn() }
}));
vi.mock('$lib/stores/scrobble.svelte', () => ({
	scrobbleManager: { init: vi.fn().mockResolvedValue(undefined) }
}));
vi.mock('$lib/utils/lazyImage', () => ({
	cancelPendingImages: vi.fn(),
	lazyImage: vi.fn(() => ({ destroy: vi.fn(), update: vi.fn() })),
	resetLazyImage: vi.fn()
}));
vi.mock('$lib/utils/requestsApi', () => ({
	fetchActiveRequestCount: vi.fn().mockResolvedValue(0),
	fetchActiveRequests: vi.fn().mockResolvedValue({ items: [] }),
	fetchRequestHistory: vi.fn().mockResolvedValue({ items: [], total: 0 })
}));
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/downloads/DownloadMutations.svelte')>()),
	requestBatch: () => ({ mutateAsync: batchRequestMock })
}));
vi.mock('$lib/utils/navigationProgress', () => ({
	createNavigationProgressController: vi.fn(() => ({
		start: vi.fn(),
		finish: vi.fn(),
		cleanup: vi.fn()
	}))
}));
vi.mock('$lib/components/Player.svelte', () => {
	shellModuleState.playerImports += 1;
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/SearchSuggestions.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/YouTubeIcon.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/DownloadsNavBadge.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/PendingApprovalNavBadge.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/lazyComponentLoaders', () => ({
	loadAuthenticatedAppShell: async () => {
		if (shellModuleState.shellFailures > 0) {
			shellModuleState.shellFailures -= 1;
			throw new Error('shell chunk unavailable');
		}
		return (await import('$lib/components/AuthenticatedAppShell.svelte')).default;
	},
	loadPlaylistModal: async () => {
		if (shellModuleState.playlistFailures > 0) {
			shellModuleState.playlistFailures -= 1;
			throw new Error('playlist chunk unavailable');
		}
		return (await import('$lib/components/AddToPlaylistModal.svelte')).default;
	},
	loadDiscographyModal: async () => {
		if (shellModuleState.discographyFailures > 0) {
			shellModuleState.discographyFailures -= 1;
			throw new Error('discography chunk unavailable');
		}
		return (await import('$lib/components/DiscographyDownloadModal.svelte')).default;
	}
}));

import Layout from './+layout.svelte';
import { integrationStore } from '$lib/stores/integration';
import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
import { nowPlayingReporter } from '$lib/stores/nowPlayingReporter.svelte';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { initCacheTTLs } from '$lib/stores/cacheTtl.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import { discographyDownloadStore } from '$lib/stores/discographyDownload.svelte';
import { batchDownloadStore } from '$lib/stores/batchDownloadStatus.svelte';
import {
	openGlobalPlaylistModal,
	playlistModalState,
	resetPlaylistModal
} from '$lib/stores/playlistModal.svelte';
import type { QueueItem } from '$lib/player/types';

type IntegrationState = {
	download_client: boolean;
	library: boolean;
	jellyfin: boolean;
	listenbrainz: boolean;
	youtube: boolean;
	localfiles: boolean;
	lastfm: boolean;
	loaded: boolean;
};

const integrationState: IntegrationState = {
	download_client: false,
	library: true,
	jellyfin: false,
	listenbrainz: false,
	youtube: false,
	localfiles: false,
	lastfm: false,
	loaded: true
};

const childrenSnippet = createRawSnippet(() => ({
	render: () => '<div data-testid="page-content">Page</div>'
}));

const playlistTrack: QueueItem = {
	trackSourceId: 'track-1',
	trackName: 'Track',
	artistName: 'Artist',
	trackNumber: 1,
	albumId: 'album-1',
	albumName: 'Album',
	coverUrl: null,
	sourceType: 'local'
};

function renderLayout() {
	return render(Layout, {
		props: { children: childrenSnippet } as Record<string, unknown>
	} as Parameters<typeof render<typeof Layout>>[1]);
}

describe('+layout.svelte sidebar', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		routeState.pathname = '/';
		shellModuleState.shellFailures = 0;
		shellModuleState.playlistFailures = 0;
		shellModuleState.discographyFailures = 0;
		resetPlaylistModal();
		discographyDownloadStore.close();
		batchDownloadStore.clear();
		batchRequestMock.mockReset();
		batchRequestMock.mockResolvedValue({
			success: false,
			requested: 0,
			skipped: 0,
			overflow: 0
		});
		authStore.clear();
		Object.assign(integrationState, {
			download_client: false,
			library: true,
			jellyfin: false,
			listenbrainz: false,
			youtube: false,
			localfiles: false,
			lastfm: false,
			loaded: true
		});
	});

	it('does not load authenticated shell modules on an auth-free route', async () => {
		routeState.pathname = '/login';
		renderLayout();

		await expect.element(page.getByTestId('page-content')).toBeVisible();
		expect(shellModuleState.playerImports).toBe(0);
	});

	it('offers a bounded retry when the authenticated shell chunk fails', async () => {
		shellModuleState.shellFailures = 1;
		renderLayout();

		await expect.element(page.getByRole('alert')).toBeVisible();
		const retry = page.getByRole('button', { name: 'Try again' });
		await expect.element(retry).toBeEnabled();

		await retry.click();
		await expect.element(page.getByTestId('page-content')).toBeVisible();
	});

	it('reports and resets a failed playlist modal chunk', async () => {
		shellModuleState.playlistFailures = 1;
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeVisible();

		openGlobalPlaylistModal([playlistTrack]);

		await vi.waitFor(() =>
			expect(playbackToast.show).toHaveBeenCalledWith(
				'Could not load the playlist dialog. Try again.',
				'error'
			)
		);
		expect(playlistModalState.shouldMount).toBe(false);
	});

	it('reports and closes a failed discography modal chunk', async () => {
		shellModuleState.discographyFailures = 1;
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeVisible();

		discographyDownloadStore.show('Artist', 'artist-1', []);

		await vi.waitFor(() =>
			expect(playbackToast.show).toHaveBeenCalledWith(
				'Could not load the discography dialog. Try again.',
				'error'
			)
		);
		expect(discographyDownloadStore.open).toBe(false);
	});

	it('does not render "Playlists" link in the sidebar when the download client is unavailable', async () => {
		renderLayout();
		await expect.element(page.getByText('Playlists')).not.toBeInTheDocument();
	});

	it('renders "Playlists" link in the sidebar when the download client is available', async () => {
		integrationState.download_client = true;
		renderLayout();
		await expect.element(page.getByText('Playlists')).toBeInTheDocument();
	});

	it('always renders "Library" link in the sidebar', async () => {
		renderLayout();
		// "Library" renders in both the desktop sidebar (first in DOM) and the mobile bottom nav, so scope to the first match for the sidebar link
		await expect.element(page.getByText('Library').first()).toBeInTheDocument();
	});

	it('uses the sole shipped dark theme', async () => {
		renderLayout();

		await expect.element(page.getByTestId('app-shell')).toHaveAttribute('data-theme', 'dark');
	});

	it('Playlists link navigates to /playlists', async () => {
		integrationState.download_client = true;
		renderLayout();
		const link = page.getByText('Playlists');
		await expect.element(link).toBeInTheDocument();
		const anchor = link.element().closest('a');
		expect(anchor).not.toBeNull();
		expect(anchor!.getAttribute('href')).toBe('/playlists');
	});

	it('Playlists link has tooltip data attribute', async () => {
		integrationState.download_client = true;
		renderLayout();
		const link = page.getByText('Playlists');
		await expect.element(link).toBeInTheDocument();
		const anchor = link.element().closest('a');
		expect(anchor!.getAttribute('data-tip')).toBe('Playlists');
	});

	it('shows the Library Management destination in a labelled admin section', async () => {
		authStore.setUser(testUser('admin'));
		renderLayout();
		await expect.element(page.getByText('Admin', { exact: true })).toBeInTheDocument();
		const link = page.getByText('Library Management').element().closest('a');
		expect(link).not.toBeNull();
		expect(link!.getAttribute('href')).toBe('/library/management');
		expect(link!.getAttribute('aria-label')).toBe('Library Management');
	});

	it('does not expose the admin navigation section to non-administrators', async () => {
		authStore.setUser(testUser('user'));
		renderLayout();
		await expect.element(page.getByText('Admin', { exact: true })).not.toBeInTheDocument();
		await expect
			.element(page.getByRole('link', { name: 'Library Management' }))
			.not.toBeInTheDocument();
	});
});

function testUser(role: AuthUser['role'] = 'user'): AuthUser {
	return {
		id: 'user-1',
		display_name: 'Test User',
		role,
		email: null,
		avatar_url: null,
		username: 'testuser',
		username_display: 'testuser',
		providers: ['local']
	};
}

describe('+layout.svelte auth-reactive session state (#155)', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		discographyDownloadStore.close();
		batchDownloadStore.clear();
		batchRequestMock.mockReset();
		batchRequestMock.mockResolvedValue({
			success: false,
			requested: 0,
			skipped: 0,
			overflow: 0
		});
		authStore.clear();
	});

	afterEach(() => {
		discographyDownloadStore.close();
		batchDownloadStore.clear();
		authStore.clear();
	});

	it('resets the integration store instead of loading it when unauthenticated', async () => {
		renderLayout();
		await vi.waitFor(() => expect(vi.mocked(integrationStore.reset)).toHaveBeenCalled());
		expect(integrationStore.ensureLoaded).not.toHaveBeenCalled();
		expect(initCacheTTLs).not.toHaveBeenCalled();
		expect(syncStatusMock.connect).not.toHaveBeenCalled();
	});

	it('loads integration status and starts session services when authenticated at mount', async () => {
		authStore.setUser(testUser());
		renderLayout();
		await vi.waitFor(() => expect(vi.mocked(integrationStore.ensureLoaded)).toHaveBeenCalled());
		expect(nowPlayingStore.start).toHaveBeenCalled();
		expect(nowPlayingReporter.start).toHaveBeenCalled();
		expect(followingEventsMock.start).toHaveBeenCalled();
		await vi.waitFor(() => expect(syncStatusMock.connect).toHaveBeenCalled());
	});

	it('loads integration status after a warm in-app login without a remount', async () => {
		renderLayout();
		await vi.waitFor(() => expect(vi.mocked(integrationStore.reset)).toHaveBeenCalled());
		expect(integrationStore.ensureLoaded).not.toHaveBeenCalled();

		authStore.setUser(testUser());
		await vi.waitFor(() => expect(vi.mocked(integrationStore.ensureLoaded)).toHaveBeenCalled());
		expect(nowPlayingStore.start).toHaveBeenCalled();
	});

	it('stops session services and resets integrations on logout', async () => {
		authStore.setUser(testUser());
		renderLayout();
		await vi.waitFor(() => expect(nowPlayingStore.start).toHaveBeenCalled());

		authStore.clear();
		await vi.waitFor(() => expect(nowPlayingStore.stop).toHaveBeenCalled());
		expect(nowPlayingReporter.stop).toHaveBeenCalled();
		expect(followingEventsMock.stop).toHaveBeenCalled();
		expect(vi.mocked(integrationStore.reset)).toHaveBeenCalled();
	});

	it('clears a pending discography selection when the account changes', async () => {
		authStore.setUser(testUser());
		renderLayout();
		await vi.waitFor(() => expect(vi.mocked(integrationStore.ensureLoaded)).toHaveBeenCalled());
		discographyDownloadStore.show('Private Artist', 'artist-a', [
			{ id: 'release-a', title: 'Private Release', requested: true }
		]);
		expect(discographyDownloadStore.open).toBe(true);

		authStore.setUser({ ...testUser(), id: 'user-2', username: 'other-user' });

		await vi.waitFor(() => expect(discographyDownloadStore.open).toBe(false));
		expect(discographyDownloadStore.artistId).toBe('');
		expect(discographyDownloadStore.releases).toEqual([]);
	});

	it('drops batch progress and a deferred discography completion after account switch', async () => {
		let resolveRequest!: (result: {
			success: boolean;
			requested: number;
			skipped: number;
			overflow: number;
		}) => void;
		batchRequestMock.mockReturnValueOnce(
			new Promise((resolve) => {
				resolveRequest = resolve;
			})
		);
		authStore.setUser(testUser());
		renderLayout();
		await vi.waitFor(() => expect(vi.mocked(integrationStore.ensureLoaded)).toHaveBeenCalled());
		batchDownloadStore.addJob('Private Artist', 'artist-a', ['release-a']);
		discographyDownloadStore.show('Private Artist', 'artist-a', [
			{ id: 'release-a', title: 'Private Release', type: 'Album' }
		]);
		const submit = page.getByRole('button', { name: 'Download 1 Album' });
		await expect.element(submit).toBeEnabled();
		await submit.click();
		await vi.waitFor(() => expect(batchRequestMock).toHaveBeenCalledOnce());

		authStore.setUser({ ...testUser(), id: 'user-2', username: 'other-user' });
		resolveRequest({ success: true, requested: 1, skipped: 0, overflow: 0 });

		await vi.waitFor(() => expect(batchDownloadStore.jobs).toEqual([]));
		expect(discographyDownloadStore.open).toBe(false);
		expect(discographyDownloadStore.releases).toEqual([]);
	});
});
