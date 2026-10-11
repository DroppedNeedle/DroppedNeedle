import { ApiError, TransportError } from '$lib/api/client';

/** How a page explains a failed read: one plain reason and what to do. */
export interface ServiceErrorView {
	kind: 'not_configured' | 'not_available' | 'rate_limited' | 'offline' | 'failed';
	title: string;
	message: string;
	/** Retrying right away can help (network blips, server errors). */
	canRetry: boolean;
	/** Where the person can fix it, when an admin setting is the cause. */
	settingsHref?: string;
}

/**
 * Turn a read error into the friendly state Home, Discover and genre pages
 * show. The server answers 503 NOT_CONFIGURED when a service needs setting
 * up, 503 NOT_AVAILABLE when this version does not have the feature, and
 * 429 (RATE_LIMITED or QUOTA_EXCEEDED) when a usage limit is spent.
 */
export function describeServiceError(error: unknown, what: string): ServiceErrorView {
	const serverMessage = error instanceof Error && error.message ? error.message : '';
	if (error instanceof TransportError) {
		return {
			kind: 'offline',
			title: "Can't reach DroppedNeedle",
			message: `${what} couldn't load because the server didn't answer. Check your connection.`,
			canRetry: true
		};
	}
	if (error instanceof ApiError) {
		if (error.status === 503 && error.code === 'NOT_CONFIGURED') {
			return {
				kind: 'not_configured',
				title: 'Not set up yet',
				message: serverMessage || `${what} needs a service that hasn't been set up.`,
				canRetry: false,
				settingsHref: '/settings'
			};
		}
		if (error.status === 503 && error.code === 'NOT_AVAILABLE') {
			return {
				kind: 'not_available',
				title: 'Not available here',
				message: serverMessage || `${what} isn't available in this version of DroppedNeedle.`,
				canRetry: false
			};
		}
		if (error.status === 429) {
			return {
				kind: 'rate_limited',
				title: 'Limit reached for now',
				message:
					serverMessage || `${what} hit a usage limit. It resets on its own; try again later.`,
				canRetry: false
			};
		}
	}
	return {
		kind: 'failed',
		title: `${what} didn't load`,
		message: serverMessage || 'Something went wrong on the server.',
		canRetry: true
	};
}
