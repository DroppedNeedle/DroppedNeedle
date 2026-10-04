import { pageFetch } from '$lib/utils/navigationAbort';
import { getApiUrl } from '$lib/api/api-utils';
import { withBasePath } from '$lib/utils/basePath';
import { browser } from '$app/environment';
import { authStore } from '$lib/stores/authStore.svelte';
import { clearUserSessionState } from '$lib/utils/userSessionCleanup';
import { createV3Client, type V3Client } from './v3/client';

export class ApiError extends Error {
	readonly status: number;
	readonly code: string;
	readonly details: unknown;

	constructor(status: number, message: string, code = '', details: unknown = null) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
		this.details = details;
	}
}

/**
 * Thrown when a request fails with 401 while a session was active. The client
 * has already cleared the store and triggered a hard redirect to /login;
 * callers can use `instanceof SessionExpiredError` to suppress error UI that
 * would otherwise flash during navigation.
 */
export class SessionExpiredError extends ApiError {
	constructor(message = 'Session expired') {
		super(401, message, 'session_expired');
		this.name = 'SessionExpiredError';
	}
}

export type TransportErrorCode = 'TRANSPORT_TIMEOUT' | 'TRANSPORT_ABORTED' | 'TRANSPORT_NETWORK';

export class TransportError extends ApiError {
	readonly method: string;
	readonly path: string;

	constructor(code: TransportErrorCode, method: string, path: string) {
		const message =
			code === 'TRANSPORT_TIMEOUT'
				? 'The request timed out'
				: code === 'TRANSPORT_ABORTED'
					? 'The request was cancelled'
					: 'The server could not be reached';
		super(0, message, code, { method, path });
		this.name = 'TransportError';
		this.method = method;
		this.path = path;
	}
}

export interface RequestOptions extends Omit<RequestInit, 'method' | 'body'> {
	signal?: AbortSignal;
	raw?: boolean;
	cache?: RequestCache;
	timeoutMs?: number;
}

interface DeleteRequestOptions extends RequestOptions {
	body?: unknown;
}

async function handleResponse<T = void>(res: Response): Promise<T> {
	if (!res.ok) {
		// Session expired mid-use: hard redirect so layout re-initialises cleanly
		if (res.status === 401 && browser && authStore.isAuthenticated) {
			await clearUserSessionState().catch(() => undefined);
			window.location.href = withBasePath('/login');
			throw new SessionExpiredError();
		}

		const text = await res.text().catch(() => '');
		let message = text || `Request failed with status ${res.status}`;
		let code = '';
		let details: unknown = null;
		try {
			const parsed = JSON.parse(text);
			if (parsed?.error?.message) {
				message = parsed.error.message;
				code = parsed.error.code ?? '';
				details = parsed.error.details ?? null;
			} else if (parsed?.detail) {
				message = parsed.detail;
			}
		} catch {
			// preserve non-JSON error bodies
		}
		throw new ApiError(res.status, message, code, details);
	}

	if (res.status === 204 || res.headers.get('content-length') === '0') {
		return undefined as T;
	}

	const text = await res.text().catch(() => '');
	if (text.trim() === '') {
		return undefined as T;
	}

	try {
		return JSON.parse(text) as T;
	} catch {
		throw new ApiError(res.status, 'Failed to parse response JSON');
	}
}

type FetchFn = typeof fetch;

interface ApiClient {
	get<T = unknown>(url: string, opts?: RequestOptions): Promise<T>;
	post<T = unknown>(url: string, body?: unknown, opts?: RequestOptions): Promise<T>;
	put<T = unknown>(url: string, body?: unknown, opts?: RequestOptions): Promise<T>;
	patch<T = unknown>(url: string, body?: unknown, opts?: RequestOptions): Promise<T>;
	delete<T = void>(url: string, opts?: DeleteRequestOptions): Promise<T>;
	head(url: string, opts?: RequestOptions): Promise<Response>;
	upload<T = unknown>(url: string, body: FormData, opts?: RequestOptions): Promise<T>;
	/** Typed v3 client over the generated contract (same fetch flavor as the host). */
	v3: V3Client;
}

function transportPath(url: string): string {
	try {
		return new URL(url, 'http://droppedneedle.invalid').pathname;
	} catch {
		return url.split('?', 1)[0] ?? url;
	}
}

/**
 * The one request pipeline: timeout/abort mapping, session-cookie fetch, and
 * the ApiError taxonomy. Both the untyped client methods and the typed v3
 * client run through here, so their failure behavior cannot drift apart.
 */
export async function executeRequest<T>(
	fetchFn: FetchFn,
	method: string,
	url: string,
	body?: unknown,
	opts?: RequestOptions
): Promise<T> {
	const { raw, timeoutMs, signal, ...fetchOpts } = opts ?? {};
	// credentials: 'include' sends the httpOnly session cookie cross-origin (dev proxy)
	const deadlineSignal = timeoutMs ? AbortSignal.timeout(timeoutMs) : undefined;
	const requestSignal =
		signal && deadlineSignal
			? AbortSignal.any([signal, deadlineSignal])
			: (signal ?? deadlineSignal);
	const init: RequestInit = {
		method,
		credentials: 'include',
		...fetchOpts,
		signal: requestSignal
	};

	if (body !== undefined && body !== null) {
		if (body instanceof FormData) {
			// Do not set Content-Type, the browser sets multipart/form-data with boundary automatically
			init.body = body;
		} else {
			const headers = new Headers(init.headers as HeadersInit | undefined);
			headers.set('Content-Type', 'application/json');
			init.headers = headers;
			init.body = JSON.stringify(body);
		}
	}

	const requestUrl = getApiUrl(url);

	let res: Response;
	try {
		res = await fetchFn(requestUrl, init);
	} catch (cause) {
		const timedOut = deadlineSignal?.aborted === true && signal?.aborted !== true;
		const aborted =
			!timedOut &&
			((cause instanceof DOMException && cause.name === 'AbortError') ||
				requestSignal?.aborted === true);
		throw new TransportError(
			timedOut ? 'TRANSPORT_TIMEOUT' : aborted ? 'TRANSPORT_ABORTED' : 'TRANSPORT_NETWORK',
			method,
			transportPath(url)
		);
	}

	if (raw) return res as unknown as T;
	return handleResponse<T>(res);
}

function createClient(fetchFn: FetchFn): ApiClient {
	return {
		get: <T = unknown>(url: string, opts?: RequestOptions) =>
			executeRequest<T>(fetchFn, 'GET', url, undefined, opts),
		post: <T = unknown>(url: string, body?: unknown, opts?: RequestOptions) =>
			executeRequest<T>(fetchFn, 'POST', url, body, opts),
		put: <T = unknown>(url: string, body?: unknown, opts?: RequestOptions) =>
			executeRequest<T>(fetchFn, 'PUT', url, body, opts),
		patch: <T = unknown>(url: string, body?: unknown, opts?: RequestOptions) =>
			executeRequest<T>(fetchFn, 'PATCH', url, body, opts),
		delete: <T = void>(url: string, opts?: DeleteRequestOptions) => {
			const { body, ...requestOptions } = opts ?? {};
			return executeRequest<T>(fetchFn, 'DELETE', url, body, requestOptions);
		},
		head: (url: string, opts?: RequestOptions) =>
			executeRequest<Response>(fetchFn, 'HEAD', url, undefined, { ...opts, raw: true }),
		upload: <T = unknown>(url: string, body: FormData, opts?: RequestOptions) =>
			executeRequest<T>(fetchFn, 'POST', url, body, opts),
		v3: createV3Client(fetchFn)
	};
}

const navClient = createClient(pageFetch);
const globalClient = createClient((...args) => globalThis.fetch(...args));

export const api = Object.assign(navClient, { global: globalClient });
