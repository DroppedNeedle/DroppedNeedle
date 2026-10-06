import type {
	Client,
	ClientPathsWithMethod,
	MethodResponse,
	RequestBodyOption
} from 'openapi-fetch';
import { executeRequest } from '../client';
import type { V3Path, V3Url } from './endpoint';
import type { paths } from './openapi';

type V3LowerMethod = 'get' | 'post' | 'put' | 'patch' | 'delete' | 'head';

/** The generated contract as an openapi-fetch client shape (JSON only, like the spec). */
type V3ClientShape = Client<paths, 'application/json'>;

type V3SuccessCodes = 200 | 201 | 202 | 203 | 205 | 206 | 207 | 208 | 226;

type V3HasSuccessBody<T extends V3Path, M extends V3LowerMethod> =
	paths[T][M] extends { responses: infer R }
		? Extract<keyof R, V3SuccessCodes> extends never
			? false
			: {
					[S in Extract<keyof R, V3SuccessCodes>]: R[S] extends {
						content: { 'application/json': unknown };
					}
						? true
						: false;
				}[Extract<keyof R, V3SuccessCodes>] extends false
				? false
				: true
		: false;

/**
 * Success data for one endpoint: void where the contract has no 2xx JSON body.
 * The constraint names the method's paths only; intersecting it with V3Path
 * here made TypeScript build a union of every path pair, which overflows its
 * 100,000-member limit once the contract passes 316 paths.
 */
export type V3Response<
	T extends ClientPathsWithMethod<V3ClientShape, M>,
	M extends V3LowerMethod
> = V3HasSuccessBody<T & V3Path, M> extends true ? MethodResponse<V3ClientShape, M, T> : void;

/** Request JSON body for one endpoint: never where the contract has none. */
export type V3Body<T extends V3Path, M extends V3LowerMethod> = RequestBodyOption<
	paths[T][M]
> extends {
	body?: infer B;
}
	? Exclude<B, undefined>
	: never;

type V3BodyArgs<T extends V3Path, M extends V3LowerMethod> = RequestBodyOption<
	paths[T][M]
> extends { body: V3Body<T, M> }
	? [body: V3Body<T, M>, opts?: V3RequestOptions]
	: [body?: V3Body<T, M>, opts?: V3RequestOptions];

export interface V3RequestOptions extends Omit<RequestInit, 'method' | 'body'> {
	signal?: AbortSignal;
	timeoutMs?: number;
}

export interface V3Client {
	GET<T extends ClientPathsWithMethod<V3ClientShape, 'get'>>(
		url: V3Url<T>,
		opts?: V3RequestOptions
	): Promise<V3Response<T, 'get'>>;
	DELETE<T extends ClientPathsWithMethod<V3ClientShape, 'delete'>>(
		url: V3Url<T>,
		opts?: V3RequestOptions
	): Promise<V3Response<T, 'delete'>>;
	HEAD<T extends ClientPathsWithMethod<V3ClientShape, 'head'>>(
		url: V3Url<T>,
		opts?: V3RequestOptions
	): Promise<V3Response<T, 'head'>>;
	POST<T extends ClientPathsWithMethod<V3ClientShape, 'post'>>(
		url: V3Url<T>,
		...args: V3BodyArgs<T, 'post'>
	): Promise<V3Response<T, 'post'>>;
	PUT<T extends ClientPathsWithMethod<V3ClientShape, 'put'>>(
		url: V3Url<T>,
		...args: V3BodyArgs<T, 'put'>
	): Promise<V3Response<T, 'put'>>;
	PATCH<T extends ClientPathsWithMethod<V3ClientShape, 'patch'>>(
		url: V3Url<T>,
		...args: V3BodyArgs<T, 'patch'>
	): Promise<V3Response<T, 'patch'>>;
}

type FetchFn = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

/**
 * Typed v3 client over the generated contract: endpoint URLs come from the
 * API registry builders, response and body types infer from each URL's brand,
 * and requests run through the shared api pipeline, so typed and untyped
 * calls share timeouts, aborts, and the ApiError taxonomy. Plain strings are
 * not endpoints; the brand is what carries the contract link.
 */
export function createV3Client(fetchFn: FetchFn): V3Client {
	return {
		GET: (url: V3Url<V3Path>, opts?: V3RequestOptions) =>
			executeRequest(fetchFn, 'GET', url, undefined, opts),
		DELETE: (url: V3Url<V3Path>, opts?: V3RequestOptions) =>
			executeRequest(fetchFn, 'DELETE', url, undefined, opts),
		HEAD: (url: V3Url<V3Path>, opts?: V3RequestOptions) =>
			executeRequest(fetchFn, 'HEAD', url, undefined, opts),
		POST: (url: V3Url<V3Path>, ...args: [body?: unknown, opts?: V3RequestOptions]) =>
			executeRequest(fetchFn, 'POST', url, args[0], args[1]),
		PUT: (url: V3Url<V3Path>, ...args: [body?: unknown, opts?: V3RequestOptions]) =>
			executeRequest(fetchFn, 'PUT', url, args[0], args[1]),
		PATCH: (url: V3Url<V3Path>, ...args: [body?: unknown, opts?: V3RequestOptions]) =>
			executeRequest(fetchFn, 'PATCH', url, args[0], args[1])
	} as unknown as V3Client;
}
