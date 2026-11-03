import type { components } from '$lib/api/v3/openapi';
import type { AuthUser } from '$lib/stores/authStore.svelte';

/** Which sign-in methods the server has switched on. */
export type AuthProviders = components['schemas']['AuthProvidersBody'];

/** User payload returned by every endpoint that establishes a session. */
export type AuthSessionUser = components['schemas']['UserResponse'];

/** A login answer's user: the native UserResponse, or the leaner federated
 * view a Plex sign-in poll returns. */
export type SessionUserLike = AuthSessionUser | components['schemas']['FederatedUserView'];

export type LocalLoginVars = components['schemas']['LoginBody'];

export type PasswordRecoveryResetVars = components['schemas']['PasswordReset'];

export type PasswordRecoveryCodeResponse = components['schemas']['RecoveryCodeResponse'];

export type JellyfinLoginVars = components['schemas']['JellyfinLoginBody'];

export type SetupVars = components['schemas']['SetupBody'];

export type OidcExchangeVars = components['schemas']['OidcExchangeBody'];

// Confirmed: the authorize POST answers the *Body schema (openapi
// /api/v3/auth/oidc/authorize 200 content), so the alias shares it by design.
export type OidcAuthorizeResponse = components['schemas']['OidcAuthorizeBody'];

/** An importable media-server account (admin import picker). */
export type ImportCandidate = components['schemas']['ImportCandidateView'];

export type ImportCandidateListResponse = components['schemas']['ImportCandidateListResponse'];

export type ImportUsersVars = components['schemas']['ImportUsersRequest'];

export type ImportUsersResult = components['schemas']['ImportUsersResponse'];

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
export function toAuthUser(user: SessionUserLike): AuthUser {
	return {
		id: user.id,
		display_name: user.display_name,
		role: toRole(user.role),
		email: user.email ?? null,
		avatar_url: user.avatar_url ?? null,
		username: user.username ?? null,
		username_display: user.username_display ?? null,
		providers: 'providers' in user ? user.providers : []
	};
}
