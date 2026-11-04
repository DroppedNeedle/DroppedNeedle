import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true }));

const authMock = vi.hoisted(() => ({ isAuthenticated: false, clear: vi.fn() }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get isAuthenticated() {
			return authMock.isAuthenticated;
		},
		clear: authMock.clear
	}
}));

const sessionCleanupMock = vi.hoisted(() => ({ run: vi.fn() }));
vi.mock('$lib/utils/userSessionCleanup', () => ({
	clearUserSessionState: sessionCleanupMock.run
}));

import { expectTypeOf } from 'vitest';
import { ApiError, SessionExpiredError, TransportError } from '../errors';
import { createV3Client, type V3Body, type V3Response } from './client';
import { v3 } from './endpoint';
import type { components } from './openapi';

const fetchMock = vi.fn();

function jsonResponse(data: unknown, status = 200): Response {
	const body = JSON.stringify(data);
	return {
		ok: status >= 200 && status < 300,
		status,
		headers: new Headers({ 'content-type': 'application/json' }),
		text: () => Promise.resolve(body),
		json: () => Promise.resolve(data)
	} as unknown as Response;
}

function emptyResponse(status = 204): Response {
	return {
		ok: status >= 200 && status < 300,
		status,
		headers: new Headers(),
		text: () => Promise.resolve(''),
		json: () => Promise.reject(new Error('no body'))
	} as unknown as Response;
}

function errorResponse(status: number, body?: unknown): Response {
	const text = body === undefined ? '' : typeof body === 'string' ? body : JSON.stringify(body);
	return {
		ok: false,
		status,
		headers: new Headers(),
		text: () => Promise.resolve(text),
		json: () =>
			body === undefined || typeof body === 'string'
				? Promise.reject(new Error('no json'))
				: Promise.resolve(body)
	} as unknown as Response;
}

beforeEach(() => {
	fetchMock.mockReset();
	authMock.isAuthenticated = false;
	authMock.clear.mockReset();
	sessionCleanupMock.run.mockReset();
	sessionCleanupMock.run.mockImplementation(async () => {
		authMock.clear();
	});
	vi.unstubAllGlobals();
});

describe('v3 GET', () => {
	it('returns data typed from the generated contract', async () => {
		fetchMock.mockResolvedValue(jsonResponse({ configured: true, reachable: true, version: null, message: 'ok' }));
		const client = createV3Client(fetchMock);

		const status: components['schemas']['SlskdStatusResponse'] = await client.GET(
			v3('/api/v3/acquire/slskd/status')
		);

		expect(status.reachable).toBe(true);
		expectTypeOf<V3Response<'/api/v3/acquire/slskd/status', 'get'>>().toEqualTypeOf<
			components['schemas']['SlskdStatusResponse']
		>();
	});

	it('sends the built URL through the shared pipeline', async () => {
		fetchMock.mockResolvedValue(jsonResponse({ id: 'job 1' }));
		const client = createV3Client(fetchMock);

		await client.GET(v3('/api/v3/library/scan/runs/{id}', { path: { id: 'job 1' } }));

		expect(fetchMock).toHaveBeenCalledWith(
			'/api/v3/library/scan/runs/job%201',
			expect.objectContaining({ method: 'GET', credentials: 'include' })
		);
	});

	it('refuses calls off the contract at build time', async () => {
		fetchMock.mockResolvedValue(jsonResponse({}));
		const client = createV3Client(fetchMock);

		// @ts-expect-error - lidarr test is POST-only
		await client.GET(v3('/api/v3/acquire/lidarr-import/test'));
		// @ts-expect-error - plain strings carry no contract brand
		await client.GET('/api/v3/acquire/slskd/status');
		// @ts-expect-error - device session mint requires a body
		await client.POST(v3('/api/v3/auth/device-sessions'));

		expect(fetchMock).toHaveBeenCalledTimes(3);
	});

});

describe('v3 DELETE', () => {
	it('resolves void for a 204 response', async () => {
		fetchMock.mockResolvedValue(emptyResponse(204));
		const client = createV3Client(fetchMock);

		const result: void = await client.DELETE(
			v3('/api/v3/me/app-passwords/{id}', { path: { id: 'ap-1' } })
		);

		expect(result).toBeUndefined();
		expectTypeOf<V3Response<'/api/v3/me/app-passwords/{id}', 'delete'>>().toEqualTypeOf<void>();
	});

});

describe('v3 POST, PUT, PATCH and HEAD', () => {
	it('sends a JSON body typed from the contract', async () => {
		fetchMock.mockResolvedValue(jsonResponse({ token: 'tok' }));
		const client = createV3Client(fetchMock);
		const body: V3Body<'/api/v3/auth/device-sessions', 'post'> = { label: 'phone' };

		await client.POST(v3('/api/v3/auth/device-sessions'), body);

		expect(fetchMock).toHaveBeenCalledWith(
			'/api/v3/auth/device-sessions',
			expect.objectContaining({ method: 'POST', body: JSON.stringify({ label: 'phone' }) })
		);
		const headers = fetchMock.mock.calls[0]![1]!.headers as Headers;
		expect(headers.get('content-type')).toContain('application/json');
	});

	it('omits the body where the contract has none', async () => {
		fetchMock.mockResolvedValue(jsonResponse({ ok: true }));
		const client = createV3Client(fetchMock);

		await client.POST(v3('/api/v3/admin/backups'));

		expect(fetchMock).toHaveBeenCalledWith(
			'/api/v3/admin/backups',
			expect.objectContaining({ method: 'POST' })
		);
		expect(fetchMock.mock.calls[0]![1]).not.toHaveProperty('body');
	});

	it('maps HEAD, PUT and PATCH to their methods', async () => {
		const client = createV3Client(fetchMock);
		const cases: [string, string, () => Promise<unknown>][] = [
			[
				'HEAD',
				'/api/v3/stream/local/k',
				(): Promise<void> =>
					client.HEAD(v3('/api/v3/stream/{source}/{*key}', { path: { source: 'local', '*key': 'k' } }))
			],
			[
				'PUT',
				'/api/v3/acquire/lidarr-import/config',
				() => client.PUT(v3('/api/v3/acquire/lidarr-import/config'), { url: '', api_key: '' })
			],
			['PATCH', '/api/v3/me', () => client.PATCH(v3('/api/v3/me'), { display_name: 'New name' })]
		];
		for (const [method, url, send] of cases) {
			fetchMock.mockReset();
			fetchMock.mockResolvedValue(method === 'HEAD' ? emptyResponse(200) : jsonResponse({ ok: true }));

			const result = await send();

			expect(fetchMock).toHaveBeenCalledWith(url, expect.objectContaining({ method }));
			if (method === 'HEAD') expect(result).toBeUndefined();
		}
	});
});

describe('v3 errors', () => {
	it('maps the error envelope to ApiError', async () => {
		fetchMock.mockResolvedValue(
			errorResponse(422, {
				error: { code: 'BAD_LABEL', message: 'Label taken', details: { label: 'x' } }
			})
		);
		const client = createV3Client(fetchMock);

		const failure = await client
			.POST(v3('/api/v3/auth/device-sessions'), { label: 'x' })
			.catch((cause: unknown) => cause);

		expect(failure).toBeInstanceOf(ApiError);
		expect(failure).toMatchObject({
			status: 422,
			message: 'Label taken',
			code: 'BAD_LABEL',
			details: { label: 'x' }
		});
	});

	it('redirects an expired session when authenticated', async () => {
		authMock.isAuthenticated = true;
		const win = { location: { href: '' } };
		vi.stubGlobal('window', win);
		fetchMock.mockResolvedValue(errorResponse(401, { error: { code: 'UNAUTH', message: 'expired' } }));
		const client = createV3Client(fetchMock);

		const failure = await client.GET(v3('/api/v3/acquire/slskd/status')).catch((cause: unknown) => cause);

		expect(failure).toBeInstanceOf(SessionExpiredError);
		expect(sessionCleanupMock.run).toHaveBeenCalledTimes(1);
		expect(win.location.href).toBe('/login');
	});

	it('maps invalid response JSON to ApiError', async () => {
		fetchMock.mockResolvedValue({
			ok: true,
			status: 200,
			headers: new Headers(),
			text: () => Promise.resolve('xx{'),
			json: () => Promise.reject(new SyntaxError('bad json'))
		} as unknown as Response);
		const client = createV3Client(fetchMock);

		const failure = await client.GET(v3('/api/v3/acquire/slskd/status')).catch((cause: unknown) => cause);

		expect(failure).toBeInstanceOf(ApiError);
		expect(failure).toMatchObject({ status: 200, message: 'Failed to parse response JSON' });
	});

	it('maps an aborted request to a transport error', async () => {
		const controller = new AbortController();
		controller.abort(new DOMException('aborted', 'AbortError'));
		fetchMock.mockRejectedValue(new DOMException('aborted', 'AbortError'));
		const client = createV3Client(fetchMock);

		const failure = await client
			.GET(v3('/api/v3/acquire/slskd/status'), { signal: controller.signal })
			.catch((cause: unknown) => cause);

		expect(failure).toBeInstanceOf(TransportError);
		expect(failure).toMatchObject({
			code: 'TRANSPORT_ABORTED',
			method: 'GET',
			path: '/api/v3/acquire/slskd/status'
		});
	});

	it('maps a deadline to a timeout without clearing the session', async () => {
		authMock.isAuthenticated = true;
		fetchMock.mockImplementation(
			(_url: string, init?: RequestInit) =>
				new Promise((_resolve, reject) => {
					init?.signal?.addEventListener('abort', () => reject(init.signal?.reason));
				})
		);
		const client = createV3Client(fetchMock);

		const failure = await client
			.GET(v3('/api/v3/acquire/slskd/status'), { timeoutMs: 1 })
			.catch((cause: unknown) => cause);

		expect(failure).toBeInstanceOf(TransportError);
		expect(failure).toMatchObject({ code: 'TRANSPORT_TIMEOUT', method: 'GET' });
		expect(authMock.clear).not.toHaveBeenCalled();
	});

	it('maps a network failure to a transport error', async () => {
		fetchMock.mockRejectedValue(new TypeError('fetch failed'));
		const client = createV3Client(fetchMock);

		const failure = await client.GET(v3('/api/v3/acquire/slskd/status')).catch((cause: unknown) => cause);

		expect(failure).toBeInstanceOf(TransportError);
		expect(failure).toMatchObject({
			code: 'TRANSPORT_NETWORK',
			method: 'GET',
			path: '/api/v3/acquire/slskd/status'
		});
	});
});
