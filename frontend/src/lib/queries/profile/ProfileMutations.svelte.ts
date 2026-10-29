import { api } from '$lib/api/client';
import { toAuthUser, type AuthSessionUser } from '$lib/queries/auth/types';
import { authStore } from '$lib/stores/authStore.svelte';
import { PROFILE_ENDPOINTS } from './endpoints';
import type {
	ChangePasswordVars,
	DisplayNameUpdateVars,
	EmailUpdateVars,
	SetPasswordVars,
	UsernameUpdateVars
} from './types';
import { createAppMutation, profileChanged } from '$lib/queries/effects';

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
		mutationFn: (vars: DisplayNameUpdateVars) => api.v3.PATCH(PROFILE_ENDPOINTS.update(), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createUpdateUsernameMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: UsernameUpdateVars) => api.v3.PUT(PROFILE_ENDPOINTS.updateUsername(), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createUpdateEmailMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: EmailUpdateVars) => api.v3.PUT(PROFILE_ENDPOINTS.updateEmail(), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createChangePasswordMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: ChangePasswordVars) => api.v3.POST(PROFILE_ENDPOINTS.changePassword(), vars),
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));

export const createSetPasswordMutation = (userId: string) =>
	createAppMutation(() => ({
		mutationFn: (vars: SetPasswordVars) => api.v3.POST(PROFILE_ENDPOINTS.setPassword(), vars),
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
			return api.v3.POST(PROFILE_ENDPOINTS.avatarUpload(), {
				content_type: file.type || 'application/octet-stream',
				image_base64
			});
		},
		onSuccess: applyUser,
		effects: () => [profileChanged(userId)]
	}));
