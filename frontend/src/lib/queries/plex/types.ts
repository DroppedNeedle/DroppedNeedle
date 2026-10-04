import type { components } from '$lib/api/v3/openapi';

// The three legs of the single Plex OAuth flow: `login` signs a session in,
// `link` attaches a media-server account to the current user, `connect`
// authorizes the server itself from admin settings.
export type PlexPurpose = 'login' | 'link' | 'connect';

export type PlexStartBody = components['schemas']['PlexStartBody'];
export type PlexLoginPollBody = components['schemas']['PlexLoginPollBody'];
export type PlexLoginPollResult = components['schemas']['PlexLoginPollResult'];
// Link and connect polls share the one-pin body; only the login leg names
// its own.
export type PlexPinBody = components['schemas']['PlexPinBody'];
export type PlexLinkPollResult = components['schemas']['PlexLinkPollResult'];
export type PlexConnectPollResult = components['schemas']['PlexConnectPollResult'];

export type PlexPollResult<P extends PlexPurpose> = P extends 'login'
	? PlexLoginPollResult
	: P extends 'link'
		? PlexLinkPollResult
		: PlexConnectPollResult;
