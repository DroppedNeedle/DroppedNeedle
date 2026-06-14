import type { AuthUser } from '$lib/stores/authStore.svelte';

export interface AuthProviders {
	local: boolean;
	plex: boolean;
	jellyfin: boolean;
	oidc: boolean;
}

/** User payload returned by every endpoint that establishes a session. */
export interface AuthSessionUser {
	id: string;
	display_name: string;
	role: string;
	email: string | null;
	avatar_url: string | null;
}

export interface AuthSessionResponse {
	user: AuthSessionUser;
}

export interface LocalLoginVars {
	email: string;
	password: string;
}

export interface JellyfinLoginVars {
	username: string;
	password: string;
}

export interface SetupVars {
	display_name: string;
	email: string;
	password: string;
}

export interface OidcExchangeVars {
	code: string;
}

export interface PlexPinResponse {
	pin_id: string;
	auth_url: string;
}

/** Poll returns `{ completed: false }` until the user authorises, then the session. */
export interface PlexPollResponse {
	completed?: boolean;
	user?: AuthSessionUser;
}

export interface OidcAuthorizeResponse {
	redirect_url: string;
}

const KNOWN_ROLES: readonly AuthUser['role'][] = ['admin', 'trusted', 'user'];

/** Validates the server-provided role, falling back to least-privilege 'user' for
 * anything unrecognised rather than trusting an arbitrary string. */
function toRole(role: string): AuthUser['role'] {
	if ((KNOWN_ROLES as readonly string[]).includes(role)) {
		return role as AuthUser['role'];
	}
	console.warn(`Unknown user role '${role}' from server; defaulting to 'user'.`);
	return 'user';
}

/** Maps a session response user onto the auth store's AuthUser shape. Centralises
 * the mapping that login, setup and the OIDC callback previously each duplicated. */
export function toAuthUser(user: AuthSessionUser): AuthUser {
	return {
		id: user.id,
		display_name: user.display_name,
		role: toRole(user.role),
		email: user.email,
		avatar_url: user.avatar_url
	};
}
