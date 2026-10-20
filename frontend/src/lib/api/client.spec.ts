import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('$lib/utils/navigationAbort', () => ({
	pageFetch: vi.fn()
}));

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

import { api, ApiError, SessionExpiredError, TransportError } from './client';
import { v3 } from './v3/endpoint';
import { pageFetch } from '$lib/utils/navigationAbort';

const HEALTH = v3('/api/v3/acquire/health');
const LOGIN = v3('/api/v3/auth/login');

const mockPageFetch = vi.mocked(pageFetch);
const mockGlobalFetch = vi.fn();
globalThis.fetch = mockGlobalFetch;

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
		ok: true,
		status,
		headers: new Headers({ 'content-length': '0' }),
		text: () => Promise.resolve(''),
		json: () => Promise.reject(new Error('no body'))
	} as unknown as Response;
}

function errorResponse(status: number, body?: unknown): Response {
	const text = body ? JSON.stringify(body) : '';
	return {
		ok: false,
		status,
		headers: new Headers(),
		text: () => Promise.resolve(text),
		json: () => (body ? Promise.resolve(body) : Promise.reject(new Error('no json')))
	} as unknown as Response;
}

beforeEach(() => {
	mockPageFetch.mockReset();
	mockGlobalFetch.mockReset();
	authMock.isAuthenticated = false;
	authMock.clear.mockReset();
	sessionCleanupMock.run.mockReset();
	sessionCleanupMock.run.mockImplementation(async () => {
		authMock.clear();
	});
	vi.unstubAllGlobals();
});

describe('api client', () => {
	describe('api.v3 (navigation-aware)', () => {
		it('calls pageFetch with GET and returns parsed JSON', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ name: 'test' }));
			const result = await api.v3.GET(HEALTH);
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v3/acquire/health',
				expect.objectContaining({ method: 'GET' })
			);
			expect(result).toEqual({ name: 'test' });
		});

		it('combines caller cancellation with a request deadline', async () => {
			const controller = new AbortController();
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));

			await api.v3.GET(HEALTH, { signal: controller.signal, timeoutMs: 10_000 });

			const init = mockPageFetch.mock.calls[0]![1]!;
			expect(init).not.toHaveProperty('timeoutMs');
			expect(init.signal).not.toBe(controller.signal);
			controller.abort();
			expect(init.signal?.aborted).toBe(true);
		});
	});

	describe('api.global.v3 (no navigation abort)', () => {
		it('calls globalThis.fetch instead of pageFetch', async () => {
			mockGlobalFetch.mockResolvedValue(jsonResponse({ name: 'global' }));
			const result = await api.global.v3.GET(HEALTH);
			expect(mockGlobalFetch).toHaveBeenCalledWith(
				'/api/v3/acquire/health',
				expect.objectContaining({ method: 'GET' })
			);
			expect(mockPageFetch).not.toHaveBeenCalled();
			expect(result).toEqual({ name: 'global' });
		});
	});

	describe('POST', () => {
		it('sends JSON body with Content-Type header', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ id: 1 }, 201));
			const result = await api.v3.POST(LOGIN, { username: 'a', password: 'b' });
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v3/auth/login',
				expect.objectContaining({
					method: 'POST',
					body: JSON.stringify({ username: 'a', password: 'b' })
				})
			);
			const sentHeaders = new Headers(mockPageFetch.mock.calls[0]![1]!.headers as HeadersInit);
			expect(sentHeaders.get('Content-Type')).toBe('application/json');
			expect(result).toEqual({ id: 1 });
		});

		it('sends no body when body is undefined', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));
			await api.v3.POST(
				v3('/api/v3/requests/approvals/{musicbrainz_id}/approve', {
					path: { musicbrainz_id: 'mbid-1' }
				})
			);
			const call = mockPageFetch.mock.calls[0];
			expect(call[1]).not.toHaveProperty('body');
		});

		it('preserves caller-supplied Headers instance when adding Content-Type', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));
			const headers = new Headers({ Authorization: 'Bearer token123' });
			await api.v3.POST(LOGIN, { username: 'a', password: 'b' }, { headers });
			const call = mockPageFetch.mock.calls[0]!;
			const sentHeaders = new Headers(call[1]!.headers as HeadersInit);
			expect(sentHeaders.get('Authorization')).toBe('Bearer token123');
			expect(sentHeaders.get('Content-Type')).toBe('application/json');
		});
	});

	describe('DELETE', () => {
		it('sends the JSON body of a revision-guarded delete', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ deleted: true }));
			await api.v3.DELETE(
				v3('/api/v3/settings/library-management/profiles/{profile_id}', {
					path: { profile_id: 'p1' }
				}),
				{ expected_settings_revision: 'revision-1' }
			);
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v3/settings/library-management/profiles/p1',
				expect.objectContaining({
					method: 'DELETE',
					body: JSON.stringify({ expected_settings_revision: 'revision-1' })
				})
			);
		});
	});

	describe('api.raw', () => {
		it('answers the raw Response without reading the body', async () => {
			const response = jsonResponse({ ignored: true });
			mockPageFetch.mockResolvedValue(response);
			const result = await api.raw('/api/v3/files/1');
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v3/files/1',
				expect.objectContaining({ method: 'GET' })
			);
			expect(result).toBe(response);
		});
	});

	describe('api.upload', () => {
		it('gives up as stalled only after the stall window passes with nothing sent', async () => {
			vi.useFakeTimers();
			const opened: { upload: { onprogress: (event: ProgressEvent) => void } }[] = [];
			class StalledXhr {
				upload = { onprogress: (_event: ProgressEvent) => {}, onload: () => {} };
				onabort = () => {};
				open() {}
				send() {
					opened.push(this);
				}
				abort() {
					this.onabort();
				}
			}
			vi.stubGlobal('XMLHttpRequest', StalledXhr);
			const sent = (loaded: number) =>
				opened[0]!.upload.onprogress({ loaded, total: 100 } as ProgressEvent);
			try {
				const outcome = api.global
					.upload(v3('/api/v3/import/uploads'), new FormData(), { stallMs: 60_000 })
					.catch((cause: unknown) => cause);
				sent(10);
				await vi.advanceTimersByTimeAsync(59_000);
				sent(20);
				await vi.advanceTimersByTimeAsync(59_000);
				const settled = await Promise.race([outcome, Promise.resolve('pending')]);
				expect(settled).toBe('pending');
				await vi.advanceTimersByTimeAsync(1_000);
				expect(await outcome).toMatchObject({
					name: 'TransportError',
					code: 'TRANSPORT_STALLED',
					path: '/api/v3/import/uploads'
				});
			} finally {
				vi.useRealTimers();
			}
		});
	});

	describe('handleResponse', () => {
		it('returns undefined for 204 responses', async () => {
			mockPageFetch.mockResolvedValue(emptyResponse(204));
			const result = await api.v3.GET(HEALTH);
			expect(result).toBeUndefined();
		});

		it('throws ApiError with backend error envelope', async () => {
			mockPageFetch.mockResolvedValue(
				errorResponse(422, {
					error: { code: 'VALIDATION', message: 'Name is required', details: { field: 'name' } }
				})
			);
			try {
				await api.v3.GET(HEALTH);
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				const err = e as ApiError;
				expect(err.status).toBe(422);
				expect(err.message).toBe('Name is required');
				expect(err.code).toBe('VALIDATION');
				expect(err.details).toEqual({ field: 'name' });
			}
		});

		it('throws ApiError with raw text when not JSON', async () => {
			mockPageFetch.mockResolvedValue(errorResponse(500, undefined));
			const res = {
				ok: false,
				status: 500,
				headers: new Headers(),
				text: () => Promise.resolve('Internal Server Error')
			} as unknown as Response;
			mockPageFetch.mockResolvedValue(res);
			try {
				await api.v3.GET(HEALTH);
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Internal Server Error');
			}
		});

		it('throws ApiError on malformed JSON body', async () => {
			const res = {
				ok: true,
				status: 200,
				headers: new Headers(),
				text: () => Promise.resolve('{malformed}')
			} as unknown as Response;
			mockPageFetch.mockResolvedValue(res);
			try {
				await api.v3.GET(HEALTH);
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Failed to parse response JSON');
			}
		});
	});

	describe('transport failures', () => {
		it('classifies a network failure with the request method and path', async () => {
			authMock.isAuthenticated = true;
			mockGlobalFetch.mockRejectedValue(new TypeError('Failed to fetch'));

			await expect(
				api.global.v3.POST(
					v3('/api/v3/requests/approvals/{musicbrainz_id}/approve', {
						path: { musicbrainz_id: 'mbid-1' },
						query: { kind: 'track' }
					})
				)
			).rejects.toMatchObject({
				name: 'TransportError',
				code: 'TRANSPORT_NETWORK',
				method: 'POST',
				path: '/api/v3/requests/approvals/mbid-1/approve',
				details: { method: 'POST', path: '/api/v3/requests/approvals/mbid-1/approve' }
			});
			expect(authMock.clear).not.toHaveBeenCalled();
		});

		it('uses the timeout transport code without clearing the session', async () => {
			authMock.isAuthenticated = true;
			mockGlobalFetch.mockImplementation(
				(_url, init) =>
					new Promise((_resolve, reject) => {
						init?.signal?.addEventListener('abort', () => reject(init.signal?.reason));
					})
			);

			try {
				await api.global.v3.GET(HEALTH, { timeoutMs: 1 });
				expect.unreachable('should have timed out');
			} catch (cause) {
				expect(cause).toBeInstanceOf(TransportError);
				expect(cause).toMatchObject({
					code: 'TRANSPORT_TIMEOUT',
					method: 'GET',
					path: '/api/v3/acquire/health'
				});
			}
			expect(authMock.clear).not.toHaveBeenCalled();
		});
	});

	describe('session expiry (401)', () => {
		it('throws SessionExpiredError, clears the store, and redirects when authenticated', async () => {
			authMock.isAuthenticated = true;
			const win = { location: { href: '' } };
			vi.stubGlobal('window', win);
			mockPageFetch.mockResolvedValue(errorResponse(401, { detail: 'expired' }));

			try {
				await api.v3.GET(HEALTH);
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(SessionExpiredError);
				expect((e as SessionExpiredError).status).toBe(401);
				expect((e as SessionExpiredError).code).toBe('session_expired');
			}
			// the Promise<T> contract is preserved by throwing
			expect(authMock.clear).toHaveBeenCalledOnce();
			expect(win.location.href).toBe('/login');
		});

		it('waits for successful session cleanup before redirecting or throwing', async () => {
			authMock.isAuthenticated = true;
			const win = { location: { href: '' } };
			let resolveCleanup!: () => void;
			const cleanup = new Promise<void>((resolve) => {
				resolveCleanup = resolve;
			});
			sessionCleanupMock.run.mockImplementationOnce(() => {
				authMock.clear();
				return cleanup;
			});
			vi.stubGlobal('window', win);
			mockPageFetch.mockResolvedValue(errorResponse(401, { detail: 'expired' }));

			const request = api.v3.GET(HEALTH);
			await vi.waitFor(() => expect(sessionCleanupMock.run).toHaveBeenCalledOnce());
			expect(authMock.clear).toHaveBeenCalledOnce();
			expect(win.location.href).toBe('');

			resolveCleanup();
			await expect(request).rejects.toBeInstanceOf(SessionExpiredError);
			expect(win.location.href).toBe('/login');
		});

		it('waits for rejected session cleanup before redirecting or throwing', async () => {
			authMock.isAuthenticated = true;
			const win = { location: { href: '' } };
			const cleanupFailure = new Error('IndexedDB unavailable');
			let rejectCleanup!: (reason: Error) => void;
			const cleanup = new Promise<void>((_, reject) => {
				rejectCleanup = reject;
			});
			sessionCleanupMock.run.mockImplementationOnce(() => {
				authMock.clear();
				return cleanup;
			});
			vi.stubGlobal('window', win);
			mockPageFetch.mockResolvedValue(errorResponse(401, { detail: 'expired' }));

			const request = api.v3.GET(HEALTH);
			await vi.waitFor(() => expect(sessionCleanupMock.run).toHaveBeenCalledOnce());
			expect(authMock.clear).toHaveBeenCalledOnce();
			expect(win.location.href).toBe('');

			rejectCleanup(cleanupFailure);
			await expect(request).rejects.toBeInstanceOf(SessionExpiredError);
			expect(win.location.href).toBe('/login');
		});

		it('observes cleanup rejection while preserving auth clearing and the login redirect', async () => {
			authMock.isAuthenticated = true;
			const win = { location: { href: '' } };
			const cleanupFailure = new Error('IndexedDB unavailable');
			sessionCleanupMock.run.mockImplementationOnce(async () => {
				authMock.clear();
				throw cleanupFailure;
			});
			vi.stubGlobal('window', win);
			mockPageFetch.mockResolvedValue(errorResponse(401, { detail: 'expired' }));

			await expect(api.v3.GET(HEALTH)).rejects.toBeInstanceOf(SessionExpiredError);
			expect(sessionCleanupMock.run).toHaveBeenCalledOnce();
			expect(authMock.clear).toHaveBeenCalledOnce();
			expect(win.location.href).toBe('/login');
		});

		it('falls through to a plain ApiError (no redirect) when unauthenticated, e.g. bad login', async () => {
			authMock.isAuthenticated = false;
			mockGlobalFetch.mockResolvedValue(
				errorResponse(401, { detail: 'Invalid email or password' })
			);

			try {
				await api.global.v3.POST(LOGIN, { username: 'a', password: 'b' });
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect(e).not.toBeInstanceOf(SessionExpiredError);
				expect((e as ApiError).message).toBe('Invalid email or password');
			}
			expect(authMock.clear).not.toHaveBeenCalled();
		});
	});
});
