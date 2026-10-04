import type { components } from '$lib/api/v3/openapi';

// One linked account in the connections aggregate. Mapped at runtime from
// the per-service v3 reads (remotes connection statuses, scrobbler link
// reads, the spotify presence probe); the encrypted secrets behind the
// links are never sent to the client (AMU-3/AMU-6). The aggregate holds
// linked accounts only: consumers treat presence as linked, so an unlinked
// service has no entry at all rather than an `enabled: false` one.
export interface ConnectionStatus {
	service: string;
	enabled: boolean;
	username: string;
}

export interface ConnectionsResponse {
	connections: ConnectionStatus[];
}

export interface LastFmAuthTokenResponse {
	token: string;
	auth_url: string;
}

// v3 answers the approved-token exchange with the link itself, not the v1
// success/message envelope.
export type LastFmAuthSessionResponse = components['schemas']['LastFmSessionResponse'];

export interface ListenBrainzConnectVars {
	user_token: string;
	username: string;
}

// media-server account links (issue #138): the password is exchanged/stored
// server-side only and never comes back in any response
export interface MediaServerConnectVars {
	username: string;
	password: string;
}

// One linked media-server account, as the connect mutations return it.
export type MediaServerConnectionStatus = components['schemas']['ConnectionStatus'];

// Adapter output: the v3 start body names the popup URL `authorize_url`, but
// the account card reads `auth_url`, so the pin mutation maps once.
export interface PlexLinkPinResponse {
	pin_id: number;
	auth_url: string;
}

export type PlexLinkPollResponse = components['schemas']['PlexLinkPollResult'];
