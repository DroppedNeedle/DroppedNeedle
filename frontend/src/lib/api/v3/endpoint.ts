import { createFinalURL, createQuerySerializer, defaultPathSerializer } from 'openapi-fetch';
import type { paths } from './openapi';

/** Every generated v3 route template. */
export type V3Path = keyof paths & string;

type ExtractPathParams<T extends string> = T extends `${string}{${infer Param}}${infer Rest}`
	? Param | ExtractPathParams<Rest>
	: never;

/** Exact path params parsed from the template: missing or spare keys fail the build. */
export type V3PathParams<T extends V3Path> = Record<ExtractPathParams<T>, string | number>;

export type V3QueryValue =
	| string
	| number
	| boolean
	| null
	| undefined
	| ReadonlyArray<string | number | boolean | null | undefined>;
export type V3Query = Record<string, V3QueryValue>;

/**
 * Params for one endpoint: path values are exact per the template, query
 * values stay a free-form record (schemas vary by method; the serializer
 * still encodes them correctly).
 */
export type V3RequestParams<T extends V3Path> = [ExtractPathParams<T>] extends [never]
	? { path?: never; query?: V3Query }
	: { path: V3PathParams<T>; query?: V3Query };

declare const v3Brand: unique symbol;

/**
 * A ready-to-send v3 URL string, branded with its generated template. It is
 * an ordinary string at runtime, so it flows through the untyped api client
 * and every string context unchanged; the typed v3 client reads the brand to
 * infer the response type.
 */
export type V3Url<T extends V3Path = V3Path> = string & { readonly [v3Brand]: T };

type V3EndpointArgs<T extends V3Path> = [ExtractPathParams<T>] extends [never]
	? [params?: V3RequestParams<T>]
	: [params: V3RequestParams<T>];

const querySerializer = createQuerySerializer();

/**
 * Build a typed endpoint URL for the v3 client. The template must be a
 * generated route and the path values must match it exactly; a missing value
 * throws rather than sending a raw `{param}` template to the server. Path and
 * query values serialize through openapi-fetch (`%20` for spaces, repeated
 * keys for arrays), which servers decode the same as `+` joins.
 */
export function v3<const T extends V3Path>(template: T, ...args: V3EndpointArgs<T>): V3Url<T> {
	const params = (args[0] ?? {}) as V3RequestParams<T>;
	const path = (params as { path?: Record<string, string | number | undefined> }).path;
	const missing = [...template.matchAll(/\{([^}]+)\}/g)]
		.map((match) => match[1] as string)
		.filter((name) => path?.[name] === undefined);
	if (missing.length > 0) {
		throw new Error(`v3 endpoint ${template} is missing path params: ${missing.join(', ')}`);
	}
	return createFinalURL(template, {
		baseUrl: '',
		params: params as { path?: Record<string, unknown>; query?: Record<string, unknown> },
		pathSerializer: defaultPathSerializer,
		querySerializer
	}) as V3Url<T>;
}
