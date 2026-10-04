import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { toAuthUser, type AuthSessionUser } from '$lib/queries/auth/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { ProfileQueryKeyFactory } from './ProfileQueryKeyFactory';
import { PROFILE_ENDPOINTS } from './endpoints';
import type {
	ChangePasswordVars,
	DisplayNameUpdateVars,
	EmailUpdateVars,
	SetPasswordVars,
	UsernameUpdateVars
} from './types';

/**
 * Profile self-service mutations (D8). Each returns the updated user
 * (`UserResponse`, incl. providers), so on success we sync `authStore` - keeping
 * the topbar name/avatar and the change-vs-set-password branch correct - and
 * invalidate the user-scoped profile query so the card refreshes (AMU-5).
 */
async function applyUser(userId: string, user: AuthSessionUser): Promise<void> {
	authStore.setUser(toAuthUser(user));
	await invalidateQueriesWithPersister({ queryKey: ProfileQueryKeyFactory.profile(userId) });
}

export const createUpdateDisplayNameMutation = (userId: string) =>
	createMutation(() => ({
		mutationFn: (vars: DisplayNameUpdateVars) =>
			api.global.v3.PATCH(PROFILE_ENDPOINTS.update(), vars),
		onSuccess: (user: AuthSessionUser) => applyUser(userId, user)
	}));

export const createUpdateUsernameMutation = (userId: string) =>
	createMutation(() => ({
		mutationFn: (vars: UsernameUpdateVars) =>
			api.global.v3.PUT(PROFILE_ENDPOINTS.updateUsername(), vars),
		onSuccess: (user: AuthSessionUser) => applyUser(userId, user)
	}));

export const createUpdateEmailMutation = (userId: string) =>
	createMutation(() => ({
		mutationFn: (vars: EmailUpdateVars) => api.global.v3.PUT(PROFILE_ENDPOINTS.updateEmail(), vars),
		onSuccess: (user: AuthSessionUser) => applyUser(userId, user)
	}));

export const createChangePasswordMutation = (userId: string) =>
	createMutation(() => ({
		mutationFn: (vars: ChangePasswordVars) =>
			api.global.v3.POST(PROFILE_ENDPOINTS.changePassword(), vars),
		onSuccess: (user: AuthSessionUser) => applyUser(userId, user)
	}));

export const createSetPasswordMutation = (userId: string) =>
	createMutation(() => ({
		mutationFn: (vars: SetPasswordVars) =>
			api.global.v3.POST(PROFILE_ENDPOINTS.setPassword(), vars),
		onSuccess: (user: AuthSessionUser) => applyUser(userId, user)
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
	createMutation(() => ({
		mutationFn: async (file: File) => {
			const image_base64 = await fileToBase64(file);
			return api.global.v3.POST(PROFILE_ENDPOINTS.avatarUpload(), {
				content_type: file.type || 'application/octet-stream',
				image_base64
			});
		},
		onSuccess: (user: AuthSessionUser) => applyUser(userId, user)
	}));
