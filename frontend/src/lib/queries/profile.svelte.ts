import { ProfileQueryKeyFactory } from '$lib/queries/featureKeys';
import type { components } from '$lib/api/v3/openapi';

import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { toAuthUser } from '$lib/queries/auth/types';
import type { AuthSessionUser } from '$lib/queries/auth/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { createAppMutation, profileChanged } from '$lib/queries/effects';

/** Own profile: identity only. The v3 backend split the old fat profile -
 *  connected services live in the connections aggregate plus the remotes
 *  connection reads, library stats in the library/remotes stats reads - so
 *  the page composes those hooks instead of one query returning everything.
 *  `providers` stays the authoritative list used to choose change- vs
 *  set-password. */
export type ProfileData = components['schemas']['ProfileResponse'];

/** One row of the profile's connected-services grid, mapped at runtime from
 *  the connections aggregate and the remotes connection reads. `url` is the
 *  server address from the connection read. The server sends it only when the
 *  caller's own account is linked to that server; otherwise it is null and the
 *  card has no link. */
export interface ProfileServiceConnection {
	name: string;
	enabled: boolean;
	username: string;
	url: string | null;
}

/** One card of the profile's libraries section, mapped at runtime from the
 *  per-source remotes stats reads (and the global library stats for local
 *  files). Sizes are null where v3 reports none; the section hides the size
 *  line then. */
export interface ProfileLibraryStats {
	source: string;
	total_tracks: number;
	total_albums: number;
	total_artists: number;
	total_size_bytes: number | null;
	total_size_human: string | null;
}

export type DisplayNameUpdateVars = components['schemas']['DisplayNameUpdate'];

export type UsernameUpdateVars = components['schemas']['UsernameUpdate'];

export type EmailUpdateVars = components['schemas']['EmailUpdate'];

export type ChangePasswordVars = components['schemas']['PasswordChange'];

export type SetPasswordVars = components['schemas']['LocalPasswordSet'];

/** The current user's profile (identity only; see `ProfileData`). Keyed by
 *  `userId` so the cache never leaks across users on a shared browser. */
export const getProfileQuery = (getUserId: Getter<string>) =>
	createQuery(() => ({
		queryKey: ProfileQueryKeyFactory.profile(getUserId()),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/me'), { signal })
	}));

/**
 * Profile self-service mutations. Each returns the updated user
 * (`UserResponse`, incl. providers), so on success we sync `authStore` - keeping
 * the topbar name/avatar and the change-vs-set-password branch correct - and
 * invalidate the user-scoped profile query so the card refreshes.
 */
function applyUser(user: AuthSessionUser): void {
	authStore.setUser(toAuthUser(user));
}

export const createUpdateDisplayNameMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: DisplayNameUpdateVars) => api.v3.PATCH(v3('/api/v3/me'), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createUpdateUsernameMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: UsernameUpdateVars) => api.v3.PUT(v3('/api/v3/me/username'), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createUpdateEmailMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: EmailUpdateVars) => api.v3.PUT(v3('/api/v3/me/email'), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createChangePasswordMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: ChangePasswordVars) => api.v3.POST(v3('/api/v3/me/password'), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createSetPasswordMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: SetPasswordVars) => api.v3.POST(v3('/api/v3/me/local-password'), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

/** v3 takes the avatar as JSON (`content_type` + base64), not multipart, so
 *  the File is encoded here and the page keeps passing a File. */
function fileToBase64(file: File): Promise<string> {
	return new Promise((resolve, reject) => {
		const reader = new FileReader();
		reader.onload = () => {
			const dataUrl = reader.result as string;
			resolve(dataUrl.slice(dataUrl.indexOf(',') + 1));
		};
		reader.onerror = () => reject(reader.error);
		reader.readAsDataURL(file);
	});
}

export const createUploadAvatarMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: async (file: File) => {
			const image_base64 = await fileToBase64(file);
			return api.v3.POST(v3('/api/v3/me/avatar'), {
				content_type: file.type || 'application/octet-stream',
				image_base64
			});
		},
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));
