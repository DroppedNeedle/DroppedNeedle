import { page } from 'vitest/browser';
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
// The shell's session-wide reads answer from fixed state, with no network.
vi.mock('$lib/queries/HomeIntegrationStatusQuery.svelte', () => ({
	getIntegrationStatusQuery: vi.fn(() => ({ data: integrationState })),
	useIntegrationStatus: () => ({ current: integrationState })
}));
vi.mock('$lib/queries/AdvancedSettingsReads.svelte', () => ({
	loadAdvancedSettings: vi.fn(() => ({ data: undefined })),
	useDirectRemoteImages: () => ({ current: true })
}));
vi.mock('$lib/stores/nowPlayingSessions.svelte', () => ({
	nowPlayingStore: { sessions: [], start: vi.fn(), stop: vi.fn() }
}));
vi.mock('$lib/stores/nowPlayingReporter.svelte', () => ({
	nowPlayingReporter: { start: vi.fn(), stop: vi.fn() }
}));
const { serverEventsMock } = vi.hoisted(() => ({
	serverEventsMock: { start: vi.fn(), stop: vi.fn(), on: vi.fn(() => () => {}) }
}));
vi.mock('$lib/queries/events/dispatcher', () => ({
	serverEvents: serverEventsMock,
	createEventDispatcher: vi.fn(() => serverEventsMock)
}));
const { muxMock } = vi.hoisted(() => ({
	muxMock: {
		connect: vi.fn(),
		disconnect: vi.fn(),
		on: vi.fn(() => () => {}),
		onConnect: vi.fn(() => () => {}),
		isConnected: false
	}
}));
vi.mock('$lib/queries/events/MuxEventStream', () => ({
	muxEventStream: muxMock,
	createMuxEventStream: vi.fn(() => muxMock)
}));
const { syncStatusMock } = vi.hoisted(() => ({
	syncStatusMock: { connect: vi.fn(), disconnect: vi.fn() }
}));
vi.mock('$lib/stores/syncStatus.svelte', () => ({ syncStatus: syncStatusMock }));
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
vi.mock('$lib/queries/downloads/DownloadMutations.svelte', () => ({
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
import { nowPlayingStore } from '$lib/stores/nowPlayingSessions.svelte';
import { nowPlayingReporter } from '$lib/stores/nowPlayingReporter.svelte';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { discographyDownloadStore } from '$lib/stores/discographyDownload.svelte';
import { batchDownloadStore } from '$lib/stores/batchDownloadStatus.svelte';
import { resetPlaylistModal } from '$lib/stores/playlistModal.svelte';

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

async function renderLayout() {
	return await render(Layout, {
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
		await renderLayout();

		await expect.element(page.getByTestId('page-content')).toBeVisible();
		expect(shellModuleState.playerImports).toBe(0);
	});

	it('shows the Library Management destination in a labelled admin section', async () => {
		authStore.setUser(testUser('admin'));
		await renderLayout();
		await expect.element(page.getByText('Admin', { exact: true })).toBeInTheDocument();
		const link = page
			.getByTestId('app-shell')
			.getByRole('link', { name: 'Library Management', exact: true, includeHidden: true })
			.first();
		await expect.element(link).toBeInTheDocument();
		await expect.element(link).toHaveAttribute('href', '/library/management');
		await expect.element(link).toHaveAttribute('aria-label', 'Library Management');
	});

	it('does not expose the admin navigation section to non-administrators', async () => {
		authStore.setUser(testUser('user'));
		await renderLayout();
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

	it('starts no session services when unauthenticated', async () => {
		await renderLayout();
		await vi.waitFor(() => expect(syncStatusMock.disconnect).toHaveBeenCalled());
		expect(syncStatusMock.connect).not.toHaveBeenCalled();
		expect(nowPlayingStore.start).not.toHaveBeenCalled();
	});

	it('starts session services when authenticated at mount', async () => {
		authStore.setUser(testUser());
		await renderLayout();
		await vi.waitFor(() => expect(nowPlayingStore.start).toHaveBeenCalled());
		expect(nowPlayingReporter.start).toHaveBeenCalled();
		expect(serverEventsMock.start).toHaveBeenCalledWith({ userId: testUser().id, isAdmin: false });
		await vi.waitFor(() => expect(syncStatusMock.connect).toHaveBeenCalled());
		await vi.waitFor(() => expect(muxMock.connect).toHaveBeenCalled());
		// the dispatcher starts before the mux connects so it sees a
		// disconnected mux and refreshes exactly once via the first open.
		const lastStart = Math.max(
			vi.mocked(serverEventsMock.start).mock.invocationCallOrder[0],
			vi.mocked(nowPlayingStore.start).mock.invocationCallOrder[0]
		);
		expect(lastStart).toBeLessThan(muxMock.connect.mock.invocationCallOrder[0]);
	});

	it('starts session services after a warm in-app login without a remount', async () => {
		await renderLayout();
		await vi.waitFor(() => expect(syncStatusMock.disconnect).toHaveBeenCalled());
		expect(nowPlayingStore.start).not.toHaveBeenCalled();

		authStore.setUser(testUser());
		await vi.waitFor(() => expect(nowPlayingStore.start).toHaveBeenCalled());
	});

	it('stops session services on logout', async () => {
		authStore.setUser(testUser());
		await renderLayout();
		await vi.waitFor(() => expect(nowPlayingStore.start).toHaveBeenCalled());

		authStore.clear();
		await vi.waitFor(() => expect(nowPlayingStore.stop).toHaveBeenCalled());
		expect(nowPlayingReporter.stop).toHaveBeenCalled();
		expect(serverEventsMock.stop).toHaveBeenCalled();
		expect(muxMock.disconnect).toHaveBeenCalled();
	});

	it('clears a pending discography selection when the account changes', async () => {
		authStore.setUser(testUser());
		await renderLayout();
		await vi.waitFor(() => expect(nowPlayingStore.start).toHaveBeenCalled());
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
		await renderLayout();
		await vi.waitFor(() => expect(nowPlayingStore.start).toHaveBeenCalled());
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
