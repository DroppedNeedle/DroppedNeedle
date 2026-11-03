import type { components } from '$lib/api/v3/openapi';

// The linked accounts from `/me/connections`. The encrypted secrets behind
// the links are never sent to the client. The query keeps enabled links
// only: consumers treat presence as linked, so an unlinked service has no
// entry at all rather than an `enabled: false` one.
export type ConnectionsResponse = components['schemas']['LinkedAccounts'];

export type LastFmAuthTokenResponse = components['schemas']['LastFmTokenResponse'];

// The approved-token exchange answers with the link itself.
export type LastFmAuthSessionResponse = components['schemas']['LastFmSessionResponse'];

export type ListenBrainzConnectVars = components['schemas']['ListenBrainzConnectRequest'];

// Media-server account links: the password is exchanged and stored
// server-side only and never comes back in any response.
export type MediaServerConnectVars = components['schemas']['ConnectionSave'];

// Adapter output: the v3 start body names the popup URL `authorize_url`, but
// the account card reads `auth_url`, so the pin mutation maps once.
export interface PlexLinkPinResponse {
	pin_id: number;
	auth_url: string;
}
