import { describe, it, expect, vi, beforeEach } from 'vitest';

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

import { api } from './client';
import { ApiError, SessionExpiredError } from './errors';
import { v3 } from './v3/endpoint';

const HEALTH = v3('/api/v3/acquire/slskd/status');
const LOGIN = v3('/api/v3/auth/login');

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
	describe('api.v3', () => {
		it('combines caller cancellation with a request deadline', async () => {
			const controller = new AbortController();
			mockGlobalFetch.mockResolvedValue(jsonResponse({ ok: true }));

			await api.v3.GET(HEALTH, { signal: controller.signal, timeoutMs: 10_000 });

			const init = mockGlobalFetch.mock.calls[0]![1]!;
			expect(init).not.toHaveProperty('timeoutMs');
			expect(init.signal).not.toBe(controller.signal);
			controller.abort();
			expect(init.signal?.aborted).toBe(true);
		});
	});

	describe('POST', () => {
		it('preserves caller-supplied Headers instance when adding Content-Type', async () => {
			mockGlobalFetch.mockResolvedValue(jsonResponse({ ok: true }));
			const headers = new Headers({ Authorization: 'Bearer token123' });
			await api.v3.POST(LOGIN, { username: 'a', password: 'b' }, { headers });
			const call = mockGlobalFetch.mock.calls[0]!;
			const sentHeaders = new Headers(call[1]!.headers as HeadersInit);
			expect(sentHeaders.get('Authorization')).toBe('Bearer token123');
			expect(sentHeaders.get('Content-Type')).toBe('application/json');
		});
	});

	describe('DELETE', () => {
		it('sends the JSON body of a revision-guarded delete', async () => {
			mockGlobalFetch.mockResolvedValue(jsonResponse({ deleted: true }));
			await api.v3.DELETE(
				v3('/api/v3/settings/library-management/profiles/{profile_id}', {
					path: { profile_id: 'p1' }
				}),
				{ expected_settings_revision: 'revision-1' }
			);
			expect(mockGlobalFetch).toHaveBeenCalledWith(
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
			mockGlobalFetch.mockResolvedValue(response);
			const result = await api.raw('/api/v3/files/1');
			expect(mockGlobalFetch).toHaveBeenCalledWith(
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
				const outcome = api
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
		it('throws ApiError with raw text when not JSON', async () => {
			mockGlobalFetch.mockResolvedValue(errorResponse(500, undefined));
			const res = {
				ok: false,
				status: 500,
				headers: new Headers(),
				text: () => Promise.resolve('Internal Server Error')
			} as unknown as Response;
			mockGlobalFetch.mockResolvedValue(res);
			try {
				await api.v3.GET(HEALTH);
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Internal Server Error');
			}
		});
	});

	describe('transport failures', () => {
		it('classifies a network failure with the request method and path', async () => {
			authMock.isAuthenticated = true;
			mockGlobalFetch.mockRejectedValue(new TypeError('Failed to fetch'));

			await expect(
				api.v3.POST(
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
	});

	describe('session expiry (401)', () => {
		const GATE_401 = {
			error: { code: 'UNAUTHORIZED', message: 'Authentication required', details: null }
		};

		it('clears the session, waits for its cleanup to settle, then throws and redirects', async () => {
			const failure = new Error('IndexedDB unavailable');
			for (const cleanup of ['done', 'resolves late', 'rejects late', 'rejects at once'] as const) {
				authMock.isAuthenticated = true;
				authMock.clear.mockClear();
				sessionCleanupMock.run.mockClear();
				const win = { location: { href: '' } };
				vi.stubGlobal('window', win);
				mockGlobalFetch.mockResolvedValue(errorResponse(401, GATE_401));
				let settle: ((ok: boolean) => void) | undefined;
				if (cleanup === 'resolves late' || cleanup === 'rejects late') {
					const pending = new Promise<void>((resolve, reject) => {
						settle = (ok) => (ok ? resolve() : reject(failure));
					});
					sessionCleanupMock.run.mockImplementationOnce(() => {
						authMock.clear();
						return pending;
					});
				} else if (cleanup === 'rejects at once') {
					sessionCleanupMock.run.mockImplementationOnce(async () => {
						authMock.clear();
						throw failure;
					});
				}

				const request = api.v3.GET(HEALTH).catch((cause: unknown) => cause);
				if (settle) {
					await vi.waitFor(() => expect(sessionCleanupMock.run).toHaveBeenCalledOnce());
					expect(authMock.clear, cleanup).toHaveBeenCalledOnce();
					expect(win.location.href, cleanup).toBe('');
					settle(cleanup === 'resolves late');
				}
				// The Promise<T> contract is kept by throwing.
				const error = await request;
				expect(error, cleanup).toBeInstanceOf(SessionExpiredError);
				expect(error, cleanup).toMatchObject({ status: 401, code: 'session_expired' });
				expect(sessionCleanupMock.run, cleanup).toHaveBeenCalledOnce();
				expect(authMock.clear, cleanup).toHaveBeenCalledOnce();
				expect(win.location.href, cleanup).toBe('/login');
			}
		});

		it('keeps the session on a 401 from anywhere but the session gate', async () => {
			authMock.isAuthenticated = true;
			const win = { location: { href: '' } };
			vi.stubGlobal('window', win);
			// A plugin route relaying its own upstream's 401.
			mockGlobalFetch.mockResolvedValueOnce(
				errorResponse(401, {
					error: { code: 'PLUGIN_AUTH', message: 'The plugin was refused', details: null }
				})
			);
			mockGlobalFetch.mockResolvedValueOnce(errorResponse(401));

			const relayed = await api.v3.GET(HEALTH).catch((cause: unknown) => cause);
			const empty = await api.v3.GET(HEALTH).catch((cause: unknown) => cause);

			expect(relayed).toBeInstanceOf(ApiError);
			expect(relayed).not.toBeInstanceOf(SessionExpiredError);
			expect(relayed).toMatchObject({ status: 401, code: 'PLUGIN_AUTH' });
			expect(empty).toBeInstanceOf(ApiError);
			expect(empty).not.toBeInstanceOf(SessionExpiredError);
			expect(empty).toMatchObject({ status: 401, code: '' });
			expect(sessionCleanupMock.run).not.toHaveBeenCalled();
			expect(win.location.href).toBe('');
		});

		it('falls through to a plain ApiError (no redirect) when unauthenticated, e.g. bad login', async () => {
			authMock.isAuthenticated = false;
			mockGlobalFetch.mockResolvedValue(
				errorResponse(401, { detail: 'Invalid email or password' })
			);

			try {
				await api.v3.POST(LOGIN, { username: 'a', password: 'b' });
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
