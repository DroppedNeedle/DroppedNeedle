import type { components } from '$lib/api/v3/openapi';
import type { V3Response } from '$lib/api/v3/client';
import type { AuthUser } from '$lib/stores/authStore.svelte';

/** Which sign-in methods the server has switched on. */
export type AuthProviders = components['schemas']['AuthProvidersBody'];

/** User payload returned by every endpoint that establishes a session. */
export type AuthSessionUser = components['schemas']['UserResponse'];

/** The least a login answer carries: the native UserResponse and the leaner
 * federated poll view both satisfy this, and it is all toAuthUser reads. */
export interface SessionUserLike {
	id: string;
	display_name: string;
	role: string;
	email?: string | null;
	avatar_url?: string | null;
	username?: string | null;
	username_display?: string | null;
	providers?: string[];
}

/** Login/setup success body, inferred from the contract (user plus the raw
 * session token only in Bearer mode. Jellyfin and OIDC logins answer the same
 * shape, but their spec entries carry no response schema, so those two
 * mutations name this type explicitly until the backend annotates them. */
export type AuthSessionResponse = V3Response<'/api/v3/auth/login', 'post'>;

export type LocalLoginVars = components['schemas']['LoginBody'];

export type PasswordRecoveryResetVars = components['schemas']['PasswordReset'];

export type PasswordRecoveryCodeResponse = components['schemas']['RecoveryCodeResponse'];

export type JellyfinLoginVars = components['schemas']['JellyfinLoginBody'];

export type SetupVars = components['schemas']['SetupBody'];

export type OidcExchangeVars = components['schemas']['OidcExchangeBody'];

// Confirmed: the authorize POST answers the *Body schema (openapi
// /api/v3/auth/oidc/authorize 200 content), so the alias shares it by design.
export type OidcAuthorizeResponse = components['schemas']['OidcAuthorizeBody'];

/** An importable media-server account (admin import picker, Phase 6 / D5). */
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
		providers: user.providers ?? []
	};
}
