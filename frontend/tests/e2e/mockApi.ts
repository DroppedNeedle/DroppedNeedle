import type { Page, Route } from '@playwright/test';

/**
 * Shared mocks for the CI browser flows (`ci-*.spec.ts`). Every flow runs
 * against a local dev server with all HTTP stubbed here: mocked providers
 * plus the dev backend only, never the live network. External hosts are
 * aborted outright, and any API call a flow forgot to stub answers 404 JSON
 * so the UI fails visibly instead of hanging.
 */

export interface MockUser {
	id: string;
	display_name: string;
	role: 'admin' | 'trusted' | 'user';
	email: string | null;
	avatar_url: string | null;
	username: string | null;
	username_display: string | null;
	providers: string[];
}

export const adminUser: MockUser = {
	id: 'admin-1',
	display_name: 'Ada Admin',
	role: 'admin',
	email: 'ada@example.com',
	avatar_url: null,
	username: 'ada',
	username_display: 'ada',
	providers: ['local']
};

export const standardUser: MockUser = {
	id: 'user-7',
	display_name: 'Jae',
	role: 'user',
	email: null,
	avatar_url: null,
	username: 'jae',
	username_display: 'Jae',
	providers: ['local']
};

export type RouteHandler = (route: Route) => Promise<void> | void;

/** Mock table key: `METHOD /path/prefix` (longest prefix wins). */
export type MockTable = Record<string, unknown | RouteHandler>;

export function isHandler(value: unknown): value is RouteHandler {
	return typeof value === 'function';
}

export async function json(route: Route, body: unknown, status = 200): Promise<void> {
	await route.fulfill({
		status,
		contentType: 'application/json',
		body: JSON.stringify(body)
	});
}

const BOOTSTRAP_DEFAULTS: MockTable = {
	'GET /api/v3/auth/setup/status': { setup_required: false },
	'GET /api/v3/auth/providers': { local: true, plex: false, jellyfin: false, oidc: false },
	'GET /api/v3/me/scrobble-preferences': { primary_music_source: 'local' },
	'GET /api/v1/home/integration-status': {
		listenbrainz: false,
		jellyfin: false,
		download_client: true,
		youtube: false,
		lastfm: false,
		navidrome: false,
		youtube_api: false,
		plex: false,
		library: true,
		localfiles: false
	},
	'GET /api/v1/home': { generated_at: 1759400000, sections: [] }
};

export interface ApiMockOptions {
	/**
	 * Session user for the bootstrap `/me` read; null mocks a logged-out
	 * browser. A getter allows flows to flip from logged-out to logged-in
	 * after the login UI posts (mirroring the session cookie surviving a
	 * full-page reload).
	 */
	user?: MockUser | null | (() => MockUser | null);
	/** Per-flow routes; override the bootstrap defaults on longest-prefix match. */
	extra?: MockTable;
	/** Observability hook for debugging unexpected calls. */
	onRequest?: (method: string, path: string) => void;
}

function matchHandler(table: MockTable, method: string, path: string): unknown {
	let best: string | null = null;
	for (const key of Object.keys(table)) {
		const [keyMethod, ...rest] = key.split(' ');
		const prefix = rest.join(' ');
		if (keyMethod !== method || !path.startsWith(prefix)) continue;
		if (best === null || key.length > best.length) best = key;
	}
	return best === null ? undefined : table[best];
}

export async function installApiMocks(page: Page, options: ApiMockOptions = {}): Promise<void> {
	const { user = adminUser, extra = {}, onRequest } = options;
	const table: MockTable = { ...BOOTSTRAP_DEFAULTS, ...extra };

	await page.route(
		(url) => url.pathname.startsWith('/api/'),
		async (route) => {
			const request = route.request();
			const url = new URL(request.url());
			onRequest?.(request.method(), url.pathname);

			if (request.method() === 'GET' && url.pathname === '/api/v3/me') {
				const session = typeof user === 'function' ? user() : user;
				if (session === null) {
					await json(route, { detail: 'missing session' }, 401);
				} else {
					await json(route, session);
				}
				return;
			}

			const found = matchHandler(table, request.method(), url.pathname);
			if (found === undefined) {
				await json(
					route,
					{ detail: `unstubbed in CI flow: ${request.method()} ${url.pathname}` },
					404
				);
				return;
			}
			if (isHandler(found)) {
				await found(route);
				return;
			}
			await json(route, found);
		}
	);

	// No live network, even by accident: anything off-loopback is aborted.
	await page.route(/https?:\/\/(?!127\.0\.0\.1|localhost)/, (route) => route.abort());
}

/** Fill the local tab and submit; resolves once the app leaves /login. */
export async function loginAsLocal(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByPlaceholder('Username').fill(username);
	await page.getByPlaceholder('Password').fill(password);
	await page.getByRole('button', { name: 'Sign in' }).click();
	await page.waitForURL((url) => !url.pathname.includes('/login'));
}
