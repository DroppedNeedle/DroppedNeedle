import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { createRawSnippet } from 'svelte';

// GH-281: the desktop sidebar had no scroll boundary, so on short viewports the
// bottom controls sat below the fold. These specs pin the viewport-bounded,
// scrollable inner nav container in both drawer states. The shell mounts through
// +layout.svelte so it runs inside the production QueryProvider, and src/app.css
// is compiled so the daisyUI drawer geometry is real.

const { routeState } = vi.hoisted(() => ({ routeState: { pathname: '/' } }));

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
	base: '/dn',
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
const playerState = { isPlayerVisible: true };
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		get isPlayerVisible() {
			return playerState.isPlayerVisible;
		},
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
vi.mock('$lib/utils/navigationProgress', () => ({
	createNavigationProgressController: vi.fn(() => ({
		start: vi.fn(),
		finish: vi.fn(),
		cleanup: vi.fn()
	}))
}));
vi.mock('$lib/components/Player.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});
vi.mock('$lib/components/SearchSuggestions.svelte', () => {
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

// The shell is loaded lazily by +layout.svelte; the dynamic imports here are the
// module-loading boundary under test, mirroring routes/layout.svelte.spec.ts.
vi.mock('$lib/components/lazyComponentLoaders', () => ({
	loadAuthenticatedAppShell: async () =>
		(await import('$lib/components/AuthenticatedAppShell.svelte')).default,
	loadPlaylistModal: async () =>
		(await import('$lib/components/AddToPlaylistModal.svelte')).default,
	loadDiscographyModal: async () =>
		(await import('$lib/components/DiscographyDownloadModal.svelte')).default
}));

import Layout from '../../routes/+layout.svelte';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { discographyDownloadStore } from '$lib/stores/discographyDownload.svelte';
import { batchDownloadStore } from '$lib/stores/batchDownloadStatus.svelte';
import { toastStore } from '$lib/stores/toast';

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
	download_client: true,
	library: true,
	jellyfin: false,
	listenbrainz: false,
	youtube: false,
	localfiles: false,
	lastfm: false,
	loaded: true
};

function testUser(role: AuthUser['role'] = 'admin'): AuthUser {
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

const childrenSnippet = createRawSnippet(() => ({
	render: () => '<div data-testid="page-content">Page</div>'
}));

function renderLayout() {
	return render(Layout, {
		props: { children: childrenSnippet } as Record<string, unknown>
	} as Parameters<typeof render<typeof Layout>>[1]);
}

/** Inner flex column of .drawer-side (child 0 is the overlay label). */
function sidebarInner(): HTMLDivElement {
	const side = document.querySelector('.drawer-side');
	if (!(side instanceof HTMLElement)) throw new Error('.drawer-side did not render');
	const inner = side.children[1];
	if (!(inner instanceof HTMLDivElement)) {
		throw new Error('inner sidebar container did not render');
	}
	return inner;
}

const VIEWPORT_H = 600;
// pb-24 on the controls container reserves clearance for the fixed player bar
const PLAYER_BAR_PX = 96;

/** Sidebar rows mount in waves (integration/service entries); wait until stable. */
async function waitForStableSidebar() {
	await vi.waitFor(
		async () => {
			const first = sidebarInner().scrollHeight;
			await new Promise((resolve) => setTimeout(resolve, 120));
			expect(sidebarInner().scrollHeight).toBe(first);
		},
		{ timeout: 8000, interval: 100 }
	);
}

/** Scroll the bounded sidebar container to its far end. */
function scrollToSidebarBottom() {
	sidebarInner().scrollTop = sidebarInner().scrollHeight;
}

describe('AuthenticatedAppShell sidebar scroll at short desktop heights (#281)', () => {
	beforeEach(async () => {
		vi.clearAllMocks();
		routeState.pathname = '/';
		playerState.isPlayerVisible = true;
		authStore.clear();
		authStore.setUser(testUser());
		await page.viewport(1280, 600);
	});

	afterEach(async () => {
		authStore.clear();
		discographyDownloadStore.close();
		batchDownloadStore.clear();
		await page.viewport(1280, 720);
	});

	it('bounds the sidebar height to the viewport with vertical overflow', async () => {
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();
		await waitForStableSidebar();

		const inner = sidebarInner();
		const style = getComputedStyle(inner);
		expect(style.overflowY).toBe('auto');
		// max-h-dvh pins the box to the 600px-tall viewport instead of growing
		expect(Number.parseFloat(style.maxHeight)).toBe(600);
		// overflow-x-hidden keeps laid-out-but-invisible collapsed tooltip bubbles
		// from adding a phantom horizontal scrollbar to the rail
		expect(style.overflowX).toBe('hidden');
	});

	it('makes the bottom sidebar controls reachable by scrolling when collapsed', async () => {
		renderLayout();
		const logout = page.getByRole('button', { name: 'Log out' });
		await expect.element(logout).toBeInTheDocument();
		await waitForStableSidebar();

		// Defect condition: without the height bound the control sits below the fold
		const before = logout.element().getBoundingClientRect();
		expect(before.bottom).toBeGreaterThan(600);

		scrollToSidebarBottom();

		// The last control (Open toggle) sits below Log out: fully scrolled it must
		// sit inside the viewport AND clear of the fixed player bar (pb-24)
		await vi.waitFor(
			() => {
				const last = lastControl();
				const rect = last.getBoundingClientRect();
				expect(rect.top).toBeGreaterThanOrEqual(0);
				expect(rect.bottom).toBeLessThanOrEqual(VIEWPORT_H - PLAYER_BAR_PX);
			},
			{ timeout: 3000 }
		);
	});

	function lastControl(): HTMLElement {
		const footer = sidebarInner().lastElementChild;
		if (!(footer instanceof HTMLElement)) {
			throw new Error('bottom controls container did not render');
		}
		// pb-24 keeps the controls clear of the fixed player bar
		expect(footer.className).toContain('pb-24');
		const control = footer.lastElementChild;
		if (!(control instanceof HTMLElement)) throw new Error('Open toggle did not render');
		return control;
	}

	it('keeps the bottom controls reachable in the expanded drawer state', async () => {
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();
		await waitForStableSidebar();

		const toggle = document.getElementById('main-drawer');
		if (!(toggle instanceof HTMLInputElement)) throw new Error('drawer toggle did not render');
		toggle.click();
		await waitForStableSidebar();

		const width = sidebarInner().getBoundingClientRect().width;
		// is-drawer-open:w-64 applies once the toggle is checked
		expect(width, `expanded sidebar width, checked=${toggle.checked}`).toBe(256);

		scrollToSidebarBottom();
		await vi.waitFor(
			() => {
				const rect = lastControl().getBoundingClientRect();
				expect(rect.top).toBeGreaterThanOrEqual(0);
				expect(rect.bottom).toBeLessThanOrEqual(VIEWPORT_H - PLAYER_BAR_PX);
			},
			{ timeout: 3000 }
		);
	});
});

// GH-182: portrait phones showed only Home/Discover/Search/Library/Settings while
// Downloads, Playlists, Requests, Following and the admin destinations existed
// only in the md+ sidebar, and non-admins got a Settings tab that bounced to
// Home. These specs pin the More overflow sheet at a 360px portrait viewport
// (the harness drives real browser CSS, so viewport control is practical and
// the md:hidden bar is genuinely visible).
describe('AuthenticatedAppShell mobile overflow menu (#182)', () => {
	beforeEach(async () => {
		vi.clearAllMocks();
		routeState.pathname = '/';
		playerState.isPlayerVisible = true;
		authStore.clear();
		await page.viewport(360, 800);
	});

	afterEach(async () => {
		authStore.clear();
		discographyDownloadStore.close();
		batchDownloadStore.clear();
		window.history.pushState({}, '', '/');
		await page.viewport(1280, 720);
	});

	function bottomNav(): HTMLElement {
		const nav = document.querySelector('.droppedneedle-bottom-nav');
		if (!(nav instanceof HTMLElement)) throw new Error('bottom nav did not render');
		return nav;
	}

	function barEntryNames(): string[] {
		return [...bottomNav().querySelectorAll(':scope > a, :scope > button')].map(
			(el) => el.textContent?.trim() ?? ''
		);
	}

	function openMoreSheet(): HTMLDialogElement {
		const more = bottomNav().querySelector('button[aria-label="More navigation options"]');
		if (!(more instanceof HTMLButtonElement)) throw new Error('More tab did not render');
		more.click();
		const sheet = document.getElementById('more_nav_sheet');
		if (!(sheet instanceof HTMLDialogElement)) throw new Error('More sheet did not render');
		return sheet;
	}

	it('exposes the missing destinations behind More with resolved hrefs', async () => {
		authStore.setUser(testUser());
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

		expect(barEntryNames()).toEqual(['Home', 'Discover', 'Search', 'Library', 'Settings', 'More']);

		const sheet = openMoreSheet();
		await vi.waitFor(() => expect(sheet.open).toBe(true));

		const hrefs = [...sheet.querySelectorAll('a')].map((a) => a.getAttribute('href'));
		// withBasePath resolves against the mocked '/dn' base
		expect(hrefs).toContain('/dn/downloads');
		expect(hrefs).toContain('/dn/following');
		expect(hrefs).toContain('/dn/playlists');
		expect(hrefs).toContain('/dn/requests');
		expect(hrefs).toContain('/dn/library/management');
		expect(hrefs).toContain('/dn/requests?tab=approvals');
	});

	it('hides Settings and admin entries from non-admins', async () => {
		authStore.setUser(testUser('user'));
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

		// No Settings tab to bounce to Home; the five-slot grid keeps even spacing.
		expect(barEntryNames()).toEqual(['Home', 'Discover', 'Search', 'Library', 'More']);
		expect(bottomNav().className).toContain('droppedneedle-bottom-nav--no-settings');

		const sheet = openMoreSheet();
		await vi.waitFor(() => expect(sheet.open).toBe(true));
		const text = sheet.textContent ?? '';
		expect(text).not.toContain('Settings');
		expect(text).not.toContain('Approvals');
		expect(text).not.toContain('Library Management');
		expect(text).toContain('Downloads');
		expect(text).toContain('Playlists');
	});

	it('fits six slots without horizontal overflow at 360px', async () => {
		authStore.setUser(testUser());
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

		const nav = bottomNav();
		await vi.waitFor(() => expect(nav.getBoundingClientRect().width).toBeGreaterThan(0));
		expect(nav.scrollWidth).toBeLessThanOrEqual(nav.clientWidth + 1);
		for (const item of nav.querySelectorAll(':scope > a, :scope > button')) {
			const el = item as HTMLElement;
			expect(el.scrollWidth, `${el.textContent?.trim()} tab overflows`).toBeLessThanOrEqual(
				el.clientWidth + 1
			);
		}
		expect(getComputedStyle(nav).gridTemplateColumns.split(' ').length).toBe(6);
	});

	it('highlights More and the matching entry when a sheet destination is active', async () => {
		authStore.setUser(testUser());
		window.history.pushState({}, '', '/dn/downloads');
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

		const more = bottomNav().querySelector('button[aria-label="More navigation options"]');
		if (!(more instanceof HTMLButtonElement)) throw new Error('More tab did not render');
		expect(more.className).toContain('active');

		const sheet = openMoreSheet();
		await vi.waitFor(() => expect(sheet.open).toBe(true));
		const downloads = sheet.querySelector('a[href="/dn/downloads"]');
		if (!(downloads instanceof HTMLAnchorElement))
			throw new Error('Downloads entry did not render');
		expect(downloads.getAttribute('aria-current')).toBe('page');
	});

	it('omits Playlists and Requests when no download client is configured', async () => {
		authStore.setUser(testUser());
		integrationState.download_client = false;
		try {
			renderLayout();
			await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

			const sheet = openMoreSheet();
			await vi.waitFor(() => expect(sheet.open).toBe(true));
			const text = sheet.textContent ?? '';
			expect(text).toContain('Downloads');
			expect(text).toContain('Following');
			expect(text).not.toContain('Playlists');
			expect(text).not.toContain('Requests');
		} finally {
			integrationState.download_client = true;
		}
	});
});

// N1: toastStore had zero subscribers, so management mutation toasts
// ("Organization preview queued" et al) never displayed. The shell now
// renders the store with role=status.
describe('AuthenticatedAppShell global toast (toastStore)', () => {
	beforeEach(async () => {
		vi.clearAllMocks();
		routeState.pathname = '/';
		playerState.isPlayerVisible = true;
		authStore.clear();
		authStore.setUser(testUser());
		toastStore.hide();
		await page.viewport(1280, 720);
	});

	afterEach(async () => {
		toastStore.hide();
		authStore.clear();
		discographyDownloadStore.close();
		batchDownloadStore.clear();
		await page.viewport(1280, 720);
	});

	function globalToast(): Element | null {
		return document.querySelector('.droppedneedle-playback-toast div[role="status"]');
	}

	it('renders a management success toast with role=status', async () => {
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

		toastStore.show({ message: 'Organization preview queued', type: 'success' });

		await vi.waitFor(() => {
			const toast = globalToast();
			expect(toast?.textContent).toContain('Organization preview queued');
		});
		expect(globalToast()?.className).toContain('alert-success');
	});

	it('renders error toasts and dismisses them', async () => {
		renderLayout();
		await expect.element(page.getByTestId('page-content')).toBeInTheDocument();

		toastStore.show({ message: 'Could not queue the management preview', type: 'error' });

		await vi.waitFor(() => {
			const toast = globalToast();
			expect(toast?.textContent).toContain('Could not queue the management preview');
		});
		expect(globalToast()?.className).toContain('alert-error');

		toastStore.hide();

		await vi.waitFor(() => {
			expect(globalToast()).toBeNull();
		});
	});
});
