/** The ids the server answers with: a UUID, or a caller's short request id (`middleware.rs`). */
const ERROR_ID = /^[A-Za-z0-9._:-]{1,128}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** The ref that ends an internal error's message: " (ref 4f1c2a9e)". */
const REF_SUFFIX = / \(ref ([A-Za-z0-9._:-]{1,128})\)$/;

export class ApiError extends Error {
	readonly status: number;
	readonly code: string;
	readonly details: unknown;
	/** The id of a 5xx internal error, which a person gives an admin; null otherwise. */
	readonly errorRef: string | null;

	constructor(status: number, message: string, code = '', details: unknown = null) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
		this.details = details;
		this.errorRef = internalErrorId(status, code, details);
	}
}

function internalErrorId(status: number, code: string, details: unknown): string | null {
	if (status < 500 || status > 599 || code !== 'INTERNAL_ERROR') return null;
	const id =
		details && typeof details === 'object'
			? (details as { error_id?: unknown }).error_id
			: undefined;
	return typeof id === 'string' && ERROR_ID.test(id) ? id : null;
}

/**
 * The message with the ref a person gives an admin, such as "Internal server
 * error (ref 4f1c2a9e)": the first 8 characters of a UUID, else the whole id.
 */
export function withErrorRef(message: string, errorId: string): string {
	const ref = UUID.test(errorId) ? errorId.slice(0, 8) : errorId;
	return `${message} (ref ${ref})`;
}

/** Splits a message that ends in a ref into its text and that ref. */
export function splitErrorRef(message: string): { text: string; ref: string | null } {
	const match = REF_SUFFIX.exec(message);
	if (!match) return { text: message, ref: null };
	return { text: message.slice(0, match.index), ref: match[1] ?? null };
}

/** The server refused a change made against a revision that has since moved. */
export function isStaleRevision(error: unknown): error is ApiError {
	return error instanceof ApiError && error.code === 'STALE_REVISION';
}

/** The server filed nothing because the song asked for is already in the library. */
export function isAlreadyInLibrary(error: unknown): error is ApiError {
	return error instanceof ApiError && error.code === 'ALREADY_IN_LIBRARY';
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

export type TransportErrorCode =
	'TRANSPORT_TIMEOUT' | 'TRANSPORT_ABORTED' | 'TRANSPORT_NETWORK' | 'TRANSPORT_STALLED';

const TRANSPORT_MESSAGES: Record<TransportErrorCode, string> = {
	TRANSPORT_TIMEOUT: 'The request timed out',
	TRANSPORT_ABORTED: 'The request was cancelled',
	TRANSPORT_NETWORK: 'The server could not be reached',
	TRANSPORT_STALLED: 'The upload stopped'
};

export class TransportError extends ApiError {
	readonly method: string;
	readonly path: string;

	constructor(code: TransportErrorCode, method: string, path: string) {
		const message = TRANSPORT_MESSAGES[code];
		super(0, message, code, { method, path });
		this.name = 'TransportError';
		this.method = method;
		this.path = path;
	}
}
