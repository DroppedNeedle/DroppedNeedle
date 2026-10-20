import { ApiError, TransportError } from '$lib/api/client';

/** How a page explains a failed read: one plain reason and what to do. */
export interface ServiceErrorView {
	kind:
		'not_configured' | 'not_available' | 'provider_down' | 'rate_limited' | 'offline' | 'failed';
	title: string;
	message: string;
	/** Retrying right away can help (network blips, server errors). */
	canRetry: boolean;
	/** Where the person can fix it, when an admin setting is the cause. */
	settingsHref?: string;
}

/** Names of the outside services the server reports by key. */
const PROVIDER_NAMES: Readonly<Record<string, string>> = {
	musicbrainz: 'MusicBrainz',
	brainzmash: 'BrainzMash',
	listenbrainz: 'ListenBrainz',
	lastfm: 'Last.fm',
	coverartarchive: 'Cover Art Archive',
	audiodb: 'TheAudioDB',
	acoustid: 'AcoustID',
	discogs: 'Discogs',
	itunes: 'iTunes',
	deezer: 'Deezer',
	lrclib: 'LRCLIB',
	wikidata: 'Wikidata',
	youtube: 'YouTube',
	ticketmaster: 'Ticketmaster',
	skiddle: 'Skiddle',
	geocoding: 'Open-Meteo',
	github: 'GitHub'
};

function detail(error: ApiError, key: string): unknown {
	const details = error.details;
	return details && typeof details === 'object' ? (details as Record<string, unknown>)[key] : null;
}

function providerName(error: ApiError): string {
	const key = detail(error, 'provider');
	if (typeof key !== 'string' || !key) return 'An outside service';
	return PROVIDER_NAMES[key] ?? key;
}

function waitPhrase(seconds: unknown): string {
	if (typeof seconds !== 'number' || !Number.isFinite(seconds) || seconds <= 0) {
		return 'Try again later.';
	}
	if (seconds < 120) {
		const whole = Math.ceil(seconds);
		return `Try again in ${whole} ${whole === 1 ? 'second' : 'seconds'}.`;
	}
	return `Try again in ${Math.ceil(seconds / 60)} minutes.`;
}

/**
 * The sentence for a 503 PROVIDER_UNAVAILABLE: which service is down and
 * when to try again, from `details.provider` and `details.retry_after`.
 * Null for any other error.
 */
export function providerUnavailableMessage(error: unknown): string | null {
	if (!(error instanceof ApiError) || error.code !== 'PROVIDER_UNAVAILABLE') return null;
	return `${providerName(error)} is not answering. ${waitPhrase(detail(error, 'retry_after'))}`;
}

/**
 * Turn a read error into the friendly state Home, Discover and genre pages
 * show. The server answers 409 NOT_CONFIGURED or PROVIDER_NOT_CONFIGURED
 * when a service needs setting up, 409 NOT_AVAILABLE when this version does
 * not have the feature, 503 PROVIDER_UNAVAILABLE when an outside service is
 * down, and 429 (RATE_LIMITED or QUOTA_EXCEEDED) when a usage limit is
 * spent.
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
		const providerDown = providerUnavailableMessage(error);
		if (providerDown) {
			return {
				kind: 'provider_down',
				title: `${what} didn't load`,
				message: providerDown,
				canRetry: true
			};
		}
		if (
			error.status === 409 &&
			(error.code === 'NOT_CONFIGURED' || error.code === 'PROVIDER_NOT_CONFIGURED')
		) {
			return {
				kind: 'not_configured',
				title: 'Not set up yet',
				message: serverMessage || `${what} needs a service that hasn't been set up.`,
				canRetry: false,
				settingsHref: '/settings'
			};
		}
		if (error.status === 409 && error.code === 'NOT_AVAILABLE') {
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
