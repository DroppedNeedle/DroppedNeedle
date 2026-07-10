import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { createRawSnippet } from 'svelte';

vi.mock('$env/dynamic/public', () => ({
	env: {
		PUBLIC_API_URL: ''
	}
}));
vi.mock('$app/environment', () => ({ browser: true, building: false, dev: false }));
vi.mock('$app/navigation', () => ({
	goto: vi.fn(),
	beforeNavigate: vi.fn(),
	afterNavigate: vi.fn(),
	onNavigate: vi.fn()
}));
vi.mock('$app/paths', () => ({
	base: '',
	assets: '',
	resolve: vi.fn((_route: string, params: Record<string, string>) => `/${params?.id ?? ''}`),
	resolveRoute: vi.fn((_route: string, params: Record<string, string>) => `/${params?.id ?? ''}`),
	asset: vi.fn((file: string) => file)
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
			cb({});
			return () => {};
		}),
		initialize: vi.fn()
	}
}));
vi.mock('$lib/stores/integration', () => ({
	integrationStore: {
		subscribe: vi.fn((cb: (v: unknown) => void) => {
			cb(integrationState);
			return () => {};
		}),
		ensureLoaded: vi.fn().mockResolvedValue(undefined)
	}
}));
vi.mock('$lib/stores/cacheTtl', () => ({ initCacheTTLs: vi.fn() }));
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
vi.mock('$lib/components/YouTubeIcon.svelte', () => {
	const Comp = function () {};
	Comp.prototype = {};
	return { default: Comp };
});

import Layout from './+layout.svelte';

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

function renderLayout() {
	return render(Layout, {
		props: { children: childrenSnippet } as Record<string, unknown>
	} as Parameters<typeof render<typeof Layout>>[1]);
}

describe('+layout.svelte sidebar', () => {
	beforeEach(() => {
		vi.clearAllMocks();
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

	it('does not render "Playlists" link in the sidebar when the download client is unavailable', async () => {
		renderLayout();
		await expect.element(page.getByText('Playlists')).not.toBeInTheDocument();
	});

	it('renders "Playlists" link in the sidebar when the download client is available', async () => {
		integrationState.download_client = true;
		renderLayout();
		// "Playlists" also renders in the command palette, so scope to the sidebar (first in DOM)
		await expect.element(page.getByText('Playlists').first()).toBeInTheDocument();
	});

	it('always renders "Library" link in the sidebar', async () => {
		renderLayout();
		// "Library" renders in both the desktop sidebar (first in DOM) and the mobile bottom nav, so scope to the first match for the sidebar link
		await expect.element(page.getByText('Library').first()).toBeInTheDocument();
	});

	it('Playlists link navigates to /playlists', async () => {
		integrationState.download_client = true;
		renderLayout();
		const link = page.getByText('Playlists').first();
		const anchor = link.element().closest('a');
		expect(anchor).not.toBeNull();
		expect(anchor!.getAttribute('href')).toBe('/playlists');
	});

	it('Playlists link has tooltip data attribute', async () => {
		integrationState.download_client = true;
		renderLayout();
		const link = page.getByText('Playlists').first();
		const anchor = link.element().closest('a');
		expect(anchor!.getAttribute('data-tip')).toBe('Playlists');
	});
});
