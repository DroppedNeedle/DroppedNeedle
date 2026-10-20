import { onTestFinished, vi } from 'vitest';
import type { paths } from '$lib/api/v3/openapi';

type Method = 'get' | 'post' | 'put' | 'patch' | 'delete';
type SuccessCode = 200 | 201 | 202 | 203 | 204 | 205 | 206 | 207 | 208 | 226;

/** `'GET /api/v3/albums/{album_id}/basic'`: a method and a contract template. */
export type RouteKey = {
	[P in keyof paths & string]: {
		[M in Method]: paths[P][M] extends { responses: unknown } ? `${Uppercase<M>} ${P}` : never;
	}[Method];
}[keyof paths & string];

type Operation<K extends RouteKey> = K extends `${infer M} ${infer P}`
	? P extends keyof paths
		? Lowercase<M> extends keyof paths[P]
			? paths[P][Lowercase<M>]
			: never
		: never
	: never;

/** The operation's 2xx JSON body, or void where it answers without one. */
export type RouteData<K extends RouteKey> =
	Operation<K> extends { responses: infer R }
		? {
				[S in keyof R & SuccessCode]: R[S] extends { content: { 'application/json': infer J } }
					? J
					: void;
			}[keyof R & SuccessCode]
		: never;

export interface FakeRequest {
	method: string;
	url: URL;
	/** Path values by template name, decoded. */
	params: Record<string, string>;
	query: URLSearchParams;
	/** The parsed JSON body, or undefined when there is none. */
	body: unknown;
	/**
	 * Uploads only: report `bytes` of the body as sent, as the browser does
	 * while an upload is in flight. The whole body counts as sent once the
	 * handler answers.
	 */
	sent?: (bytes: number) => void;
}

const replyBrand = Symbol('fakeApiReply');

/** A non-2xx answer, built by `apiError` or `textReply`. A string body is sent as text. */
export interface FakeReply {
	readonly [replyBrand]: true;
	status: number;
	body: unknown;
}

/** An error answer in the server's envelope: `{error: {code, message, details}}`. */
export function apiError(
	status: number,
	code: string,
	message = 'Request failed',
	details: unknown = null
): FakeReply {
	return { [replyBrand]: true, status, body: { error: { code, message, details } } };
}

/** A non-2xx answer with a plain text body, as a proxy in front of the server sends. */
export function textReply(status: number, text: string): FakeReply {
	return { [replyBrand]: true, status, body: text };
}

type Answer<K extends RouteKey> = RouteData<K> | FakeReply;

export type RouteHandler<K extends RouteKey> =
	Answer<K> | ((request: FakeRequest) => Answer<K> | Promise<Answer<K>>);

export type FakeRoutes = { [K in RouteKey]?: RouteHandler<K> };

export interface FakeApi {
	/** Requests answered so far, optionally only those of one route. */
	calls(route?: RouteKey): FakeRequest[];
	/** Add or replace routes for the rest of the test. */
	use(routes: FakeRoutes): void;
}

interface CompiledRoute {
	key: RouteKey;
	method: string;
	pattern: RegExp;
	names: string[];
	literalLength: number;
}

function compile(key: RouteKey): CompiledRoute {
	const space = key.indexOf(' ');
	const method = key.slice(0, space);
	const template = key.slice(space + 1);
	const names: string[] = [];
	let literalLength = 0;
	const source = template
		.split(/(\{[^}]+\})/)
		.map((part) => {
			const name = /^\{([^}]+)\}$/.exec(part)?.[1];
			if (name === undefined) {
				literalLength += part.length;
				return part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
			}
			names.push(name.replace(/^\*/, ''));
			return name.startsWith('*') ? '(.+)' : '([^/]+)';
		})
		.join('');
	return { key, method, pattern: new RegExp(`^${source}$`), names, literalLength };
}

function isReply(value: unknown): value is FakeReply {
	return typeof value === 'object' && value !== null && replyBrand in value;
}

function jsonResponse(status: number, body: unknown): Response {
	if (body === undefined) return new Response(null, { status: status === 200 ? 204 : status });
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

async function readBody(body: BodyInit | null | undefined): Promise<unknown> {
	if (typeof body !== 'string' || body === '') return undefined;
	return JSON.parse(body);
}

type Transport = (
	input: RequestInfo | URL,
	init?: RequestInit,
	sent?: (bytes: number) => void
) => Promise<Response>;

function bodySize(body: unknown): number {
	if (!(body instanceof FormData)) return typeof body === 'string' ? body.length : 0;
	let size = 0;
	for (const [, value] of body.entries()) {
		size += typeof value === 'string' ? value.length : value.size;
	}
	return size;
}

/**
 * The part of XMLHttpRequest the api client's upload uses, answered by the
 * same routes as fetch. Upload progress events follow `request.sent`, then
 * the whole body once the handler answers.
 */
function fakeXhr(transport: Transport) {
	return class FakeXMLHttpRequest {
		status = 0;
		responseText = '';
		withCredentials = false;
		readonly upload: {
			onprogress: ((event: ProgressEvent) => void) | null;
			onload: (() => void) | null;
		} = { onprogress: null, onload: null };
		onload: (() => void) | null = null;
		onerror: (() => void) | null = null;
		ontimeout: (() => void) | null = null;
		onabort: (() => void) | null = null;
		private method = 'GET';
		private url = '';
		private done = false;

		open(method: string, url: string): void {
			this.method = method;
			this.url = url;
		}

		abort(): void {
			if (this.done) return;
			this.done = true;
			this.onabort?.();
		}

		send(body?: Document | XMLHttpRequestBodyInit | null): void {
			const total = bodySize(body);
			const progress = (loaded: number) => {
				if (this.done) return;
				this.upload.onprogress?.(
					new ProgressEvent('progress', { loaded, total, lengthComputable: true })
				);
			};
			void transport(this.url, { method: this.method, body: body as BodyInit }, progress).then(
				async (response) => {
					if (this.done) return;
					progress(total);
					this.upload.onload?.();
					this.status = response.status;
					this.responseText = await response.text();
					this.done = true;
					this.onload?.();
				}
			);
		}
	};
}

/**
 * Replace the network under the real api client for the current test. Each
 * request is matched by method and contract template and answered by its
 * handler, whose return type is that operation's 2xx JSON. A request no route
 * matches answers 501 and fails the test when it ends, naming the method and
 * URL. The transport is restored when the test finishes.
 */
export function fakeApi(routes: FakeRoutes): FakeApi {
	const handlers = new Map<RouteKey, unknown>();
	let compiled: CompiledRoute[] = [];
	const answered: { key: RouteKey; request: FakeRequest }[] = [];
	const unmatched: string[] = [];

	function use(next: FakeRoutes): void {
		for (const [key, handler] of Object.entries(next)) {
			handlers.set(key as RouteKey, handler);
		}
		// The most literal template wins, so `/albums/recent` beats `/albums/{id}`.
		compiled = [...handlers.keys()].map(compile).sort((a, b) => b.literalLength - a.literalLength);
	}
	use(routes);

	const transport: Transport = async (input, init, sent) => {
		const request = input instanceof Request ? input : undefined;
		const method = (init?.method ?? request?.method ?? 'GET').toUpperCase();
		const url = new URL(
			request ? request.url : input instanceof URL ? input.href : String(input),
			window.location.origin
		);
		for (const route of compiled) {
			if (route.method !== method) continue;
			const match = route.pattern.exec(url.pathname);
			if (!match) continue;
			const params: Record<string, string> = {};
			route.names.forEach((name, i) => {
				params[name] = decodeURIComponent(match[i + 1] ?? '');
			});
			const fake: FakeRequest = {
				method,
				url,
				params,
				query: url.searchParams,
				body: await readBody(init?.body ?? (request ? await request.text() : undefined)),
				sent
			};
			answered.push({ key: route.key, request: fake });
			const handler = handlers.get(route.key);
			const answer = typeof handler === 'function' ? await handler(fake) : handler;
			if (!isReply(answer)) return jsonResponse(200, answer);
			return typeof answer.body === 'string'
				? new Response(answer.body, { status: answer.status })
				: jsonResponse(answer.status, answer.body);
		}
		unmatched.push(`${method} ${url.pathname}${url.search}`);
		return jsonResponse(501, {
			error: { code: 'NOT_FAKED', message: `No fake for ${method} ${url.pathname}`, details: null }
		});
	};

	vi.stubGlobal('fetch', vi.fn(transport));
	vi.stubGlobal('XMLHttpRequest', fakeXhr(transport));
	onTestFinished(() => {
		vi.unstubAllGlobals();
		if (unmatched.length > 0) {
			throw new Error(`fakeApi: no route for\n  ${[...new Set(unmatched)].join('\n  ')}`);
		}
	});

	return {
		calls: (route) =>
			answered.filter((call) => !route || call.key === route).map((call) => call.request),
		use
	};
}
