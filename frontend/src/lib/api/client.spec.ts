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

import { api, ApiError, SessionExpiredError } from './client';
import { pageFetch } from '$lib/utils/navigationAbort';

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
	vi.unstubAllGlobals();
});

describe('api client', () => {
	describe('api.get (navigation-aware)', () => {
		it('calls pageFetch with GET and returns parsed JSON', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ name: 'test' }));
			const result = await api.get<{ name: string }>('/api/v1/test');
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/test',
				expect.objectContaining({ method: 'GET' })
			);
			expect(result).toEqual({ name: 'test' });
		});

		it('passes signal through', async () => {
			const controller = new AbortController();
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));
			await api.get('/api/v1/test', { signal: controller.signal });
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/test',
				expect.objectContaining({ signal: controller.signal })
			);
		});

		it('passes cache option through', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));
			await api.get('/api/v1/test', { cache: 'no-cache' });
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/test',
				expect.objectContaining({ cache: 'no-cache' })
			);
		});
	});

	describe('api.global.get (no navigation abort)', () => {
		it('calls globalThis.fetch instead of pageFetch', async () => {
			mockGlobalFetch.mockResolvedValue(jsonResponse({ name: 'global' }));
			const result = await api.global.get<{ name: string }>('/api/v1/global');
			expect(mockGlobalFetch).toHaveBeenCalledWith(
				'/api/v1/global',
				expect.objectContaining({ method: 'GET' })
			);
			expect(mockPageFetch).not.toHaveBeenCalled();
			expect(result).toEqual({ name: 'global' });
		});
	});

	describe('api.post', () => {
		it('sends JSON body with Content-Type header', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ id: 1 }, 201));
			const result = await api.post<{ id: number }>('/api/v1/items', { name: 'new' });
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/items',
				expect.objectContaining({
					method: 'POST',
					body: JSON.stringify({ name: 'new' })
				})
			);
			const sentHeaders = new Headers(mockPageFetch.mock.calls[0]![1]!.headers as HeadersInit);
			expect(sentHeaders.get('Content-Type')).toBe('application/json');
			expect(result).toEqual({ id: 1 });
		});

		it('sends no body when body is undefined', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));
			await api.post('/api/v1/trigger');
			const call = mockPageFetch.mock.calls[0];
			expect(call[1]).not.toHaveProperty('body');
		});

		it('preserves caller-supplied Headers instance when adding Content-Type', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ ok: true }));
			const headers = new Headers({ Authorization: 'Bearer token123' });
			await api.post('/api/v1/data', { key: 'val' }, { headers });
			const call = mockPageFetch.mock.calls[0]!;
			const sentHeaders = new Headers(call[1]!.headers as HeadersInit);
			expect(sentHeaders.get('Authorization')).toBe('Bearer token123');
			expect(sentHeaders.get('Content-Type')).toBe('application/json');
		});
	});

	describe('api.put', () => {
		it('sends PUT with JSON body', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ updated: true }));
			await api.put('/api/v1/items/1', { name: 'updated' });
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/items/1',
				expect.objectContaining({ method: 'PUT' })
			);
		});
	});

	describe('api.patch', () => {
		it('sends PATCH with JSON body', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ patched: true }));
			await api.patch('/api/v1/items/1', { name: 'patched' });
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/items/1',
				expect.objectContaining({ method: 'PATCH' })
			);
		});
	});

	describe('api.delete', () => {
		it('sends DELETE and handles 204', async () => {
			mockPageFetch.mockResolvedValue(emptyResponse(204));
			await api.delete('/api/v1/items/1');
			expect(mockPageFetch).toHaveBeenCalledWith(
				'/api/v1/items/1',
				expect.objectContaining({ method: 'DELETE' })
			);
		});

		it('returns typed JSON for 200 DELETE responses', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ success: true, artist_removed: true }));
			const data = await api.delete<{ success: boolean; artist_removed: boolean }>(
				'/api/v1/items/1'
			);
			expect(data).toEqual({ success: true, artist_removed: true });
		});
	});

	describe('api.head', () => {
		it('returns raw Response without parsing', async () => {
			const rawRes = jsonResponse({}, 200);
			mockPageFetch.mockResolvedValue(rawRes);
			const result = await api.head('/api/v1/stream/123');
			expect(result).toBe(rawRes);
		});
	});

	describe('api.global.head', () => {
		it('uses global fetch for HEAD', async () => {
			const rawRes = jsonResponse({}, 200);
			mockGlobalFetch.mockResolvedValue(rawRes);
			const result = await api.global.head('/api/v1/stream/123');
			expect(result).toBe(rawRes);
			expect(mockPageFetch).not.toHaveBeenCalled();
		});
	});

	describe('api.upload', () => {
		it('sends FormData without Content-Type header', async () => {
			mockPageFetch.mockResolvedValue(jsonResponse({ url: '/cover.jpg' }));
			const formData = new FormData();
			formData.append('file', new Blob(['test']), 'test.jpg');
			const result = await api.upload<{ url: string }>('/api/v1/upload', formData);
			const call = mockPageFetch.mock.calls[0]!;
			expect(call[1]!.body).toBe(formData);
			expect(call[1]!.headers).toBeUndefined();
			expect(result).toEqual({ url: '/cover.jpg' });
		});
	});

	describe('handleResponse', () => {
		it('returns undefined for 204 responses', async () => {
			mockPageFetch.mockResolvedValue(emptyResponse(204));
			const result = await api.get('/api/v1/empty');
			expect(result).toBeUndefined();
		});

		it('returns undefined for content-length: 0', async () => {
			mockPageFetch.mockResolvedValue(emptyResponse(200));
			const result = await api.get('/api/v1/empty');
			expect(result).toBeUndefined();
		});

		it('returns undefined for empty body text', async () => {
			const res = {
				ok: true,
				status: 200,
				headers: new Headers(),
				text: () => Promise.resolve('  ')
			} as unknown as Response;
			mockPageFetch.mockResolvedValue(res);
			const result = await api.get('/api/v1/empty');
			expect(result).toBeUndefined();
		});

		it('throws ApiError with backend error envelope', async () => {
			mockPageFetch.mockResolvedValue(
				errorResponse(422, {
					error: { code: 'VALIDATION', message: 'Name is required', details: { field: 'name' } }
				})
			);
			try {
				await api.get('/api/v1/test');
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

		it('throws ApiError with detail field (FastAPI style)', async () => {
			mockPageFetch.mockResolvedValue(errorResponse(400, { detail: 'Bad request' }));
			try {
				await api.get('/api/v1/test');
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Bad request');
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
				await api.get('/api/v1/test');
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Internal Server Error');
			}
		});

		it('throws ApiError with fallback message when text() fails', async () => {
			const res = {
				ok: false,
				status: 503,
				headers: new Headers(),
				text: () => Promise.resolve('')
			} as unknown as Response;
			mockPageFetch.mockResolvedValue(res);
			try {
				await api.get('/api/v1/test');
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Request failed with status 503');
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
				await api.get('/api/v1/test');
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(ApiError);
				expect((e as ApiError).message).toBe('Failed to parse response JSON');
			}
		});
	});

	describe('ApiError', () => {
		it('extends Error with correct name', () => {
			const err = new ApiError(404, 'Not found', 'NOT_FOUND');
			expect(err).toBeInstanceOf(Error);
			expect(err.name).toBe('ApiError');
			expect(err.status).toBe(404);
			expect(err.message).toBe('Not found');
			expect(err.code).toBe('NOT_FOUND');
		});
	});

	describe('session expiry (401)', () => {
		it('throws SessionExpiredError, clears the store, and redirects when authenticated', async () => {
			authMock.isAuthenticated = true;
			const win = { location: { href: '' } };
			vi.stubGlobal('window', win);
			mockPageFetch.mockResolvedValue(errorResponse(401, { detail: 'expired' }));

			try {
				await api.get('/api/v1/auth/me');
				expect.unreachable('should have thrown');
			} catch (e) {
				expect(e).toBeInstanceOf(SessionExpiredError);
				expect((e as SessionExpiredError).status).toBe(401);
				expect((e as SessionExpiredError).code).toBe('session_expired');
			}
			// Never returns undefined — the Promise<T> contract is honoured by throwing.
			expect(authMock.clear).toHaveBeenCalledOnce();
			expect(win.location.href).toBe('/login');
		});

		it('falls through to a plain ApiError (no redirect) when unauthenticated, e.g. bad login', async () => {
			authMock.isAuthenticated = false;
			mockGlobalFetch.mockResolvedValue(
				errorResponse(401, { detail: 'Invalid email or password' })
			);

			try {
				await api.global.post('/api/v1/auth/login', { email: 'a', password: 'b' });
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
