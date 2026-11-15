// The desktop rail at short viewports (#723): its destinations scroll inside
// it, while the expand and log out controls stay on screen instead of being
// cut off below the fold. The rail's own layout is what is under test, so the
// drawer wrapper is built the way the shell renders it (the toggle input and
// the content column as siblings) and the real app.css is compiled in for the
// daisyUI drawer geometry.
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi, onTestFinished } from 'vitest';
import { mount, unmount } from 'svelte';
import '../../app.css';

vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/library') }
}));
vi.mock('$app/navigation', () => ({ goto: vi.fn() }));
vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: {
		isPlayerVisible: false,
		isPlaying: false,
		playbackState: 'idle',
		nowPlaying: null,
		progress: 0,
		duration: 0
	}
}));
vi.mock('$lib/features/player/state/nowPlayingSessions.svelte', () => ({
	nowPlayingStore: { sessions: [], start: vi.fn(), stop: vi.fn(), refresh: vi.fn() }
}));
vi.mock('$lib/features/player/state/nowPlayingMerged.svelte', () => ({
	nowPlayingMerged: {
		sessions: [],
		activeSessions: [],
		primarySession: null,
		isSourcePlaying: () => false,
		sourceHasSessions: () => false,
		sessionsForSource: () => [],
		start: vi.fn(),
		stop: vi.fn(),
		refresh: vi.fn()
	}
}));
vi.mock('$lib/features/home/queries/home.svelte', () => {
	const integrations = {
		download_client: true,
		library: true,
		jellyfin: true,
		listenbrainz: false,
		youtube: false,
		localfiles: true,
		lastfm: false,
		loaded: true
	};
	return {
		getIntegrationStatusQuery: vi.fn(() => ({ data: integrations })),
		useIntegrationStatus: () => ({ current: integrations })
	};
});
vi.mock('$lib/features/settings/queries/section-prefs.svelte', () => ({
	getSectionPrefsQuery: () => ({ data: undefined })
}));
// The badges read their own queries; their pills do not change the rail's height.
const { EmptyBadge } = vi.hoisted(() => ({ EmptyBadge: function EmptyBadge() {} }));
vi.mock('$lib/shell/DownloadsNavBadge.svelte', () => ({ default: EmptyBadge }));
vi.mock('$lib/shell/PendingApprovalNavBadge.svelte', () => ({ default: EmptyBadge }));
vi.mock('$lib/shell/ConcertsNavBadge.svelte', () => ({ default: EmptyBadge }));
vi.mock('$lib/shell/NewReleasesNavBadge.svelte', () => ({ default: EmptyBadge }));
vi.mock('$lib/shell/CheckAlbumsNavBadge.svelte', () => ({ default: EmptyBadge }));

import SidebarNav from './SidebarNav.svelte';
import { authStore } from '$lib/stores/authStore.svelte';

const VIEWPORT_H = 700;
const VIEWPORT_W = 1100;

function renderShell() {
	const drawer = document.createElement('div');
	drawer.className = 'drawer md:drawer-open';
	const toggle = document.createElement('input');
	toggle.id = 'main-drawer';
	toggle.type = 'checkbox';
	toggle.className = 'drawer-toggle';
	toggle.setAttribute('aria-label', 'Expand sidebar');
	// a page taller than the viewport, so the rail has to bound itself
	const content = document.createElement('div');
	content.className = 'drawer-content flex min-w-0 flex-col';
	const page = document.createElement('div');
	page.style.height = '2400px';
	content.append(page);
	drawer.append(toggle, content);
	document.body.append(drawer);
	const nav = mount(SidebarNav, {
		target: drawer,
		props: {
			currentPath: '/library',
			viewer: { isAdmin: true, isCurator: true },
			updateAvailable: false,
			onsearch: () => {}
		}
	});
	onTestFinished(() => {
		unmount(nav);
		drawer.remove();
	});
	return drawer;
}

function rail(): HTMLElement {
	return document.querySelector('.dn-rail') as HTMLElement;
}

function destinations(): HTMLElement {
	return rail().querySelector('ul.menu') as HTMLElement;
}

function control(label: string): HTMLElement {
	return rail().querySelector(`[aria-label="${label}"]`) as HTMLElement;
}

/** The control is fully on screen and nothing covers its centre. */
function reachable(label: string): boolean {
	const element = control(label);
	const box = element.getBoundingClientRect();
	const onScreen =
		box.top >= 0 && box.bottom <= innerHeight && box.left >= 0 && box.right <= innerWidth;
	const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
	return onScreen && hit !== null && (hit === element || element.contains(hit));
}

async function waitFor(width: number) {
	await vi.waitFor(() => expect(rail().getBoundingClientRect().width).toBe(width));
}

describe('SidebarNav at a short viewport', () => {
	afterEach(() => {
		authStore.clear();
	});

	async function open(collapsed: boolean) {
		await page.viewport(VIEWPORT_W, VIEWPORT_H);
		authStore.setUser({
			id: 'user-1',
			display_name: 'Test User',
			role: 'admin',
			email: null,
			avatar_url: null,
			username: 'testuser',
			username_display: 'testuser',
			providers: ['local']
		});
		const drawer = renderShell();
		if (!collapsed) (drawer.querySelector('.drawer-toggle') as HTMLInputElement).click();
		await waitFor(collapsed ? 56 : 240);
	}

	it('keeps the footer controls on screen while the destinations scroll', async () => {
		await open(true);

		expect(rail().getBoundingClientRect().height).toBe(VIEWPORT_H);
		expect(reachable('Toggle sidebar')).toBe(true);
		expect(reachable('Log out')).toBe(true);

		expect(destinations().scrollHeight).toBeGreaterThan(destinations().clientHeight);
		destinations().scrollTop = destinations().scrollHeight;

		expect(reachable('Toggle sidebar')).toBe(true);
		expect(reachable('Log out')).toBe(true);
	});

	it('keeps the footer controls on screen with the sidebar expanded', async () => {
		await open(false);

		expect(rail().getBoundingClientRect().width).toBe(240);
		expect(rail().getBoundingClientRect().height).toBe(VIEWPORT_H);
		expect(reachable('Toggle sidebar')).toBe(true);
		expect(reachable('Log out')).toBe(true);
	});
});
