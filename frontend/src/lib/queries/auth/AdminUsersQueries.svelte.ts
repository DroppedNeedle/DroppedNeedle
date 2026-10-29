import { createQuery, keepPreviousData } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { V3Body } from '$lib/api/v3/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { createAppMutation, usersChanged } from '../effects';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

export type AdminUser = components['schemas']['UserResponse'];
type CreateUserBody = V3Body<'/api/v3/admin/users', 'post'>;
type RoleBody = V3Body<'/api/v3/admin/users/{id}/role', 'put'>;

const refreshUsers = () => [usersChanged(authStore.user?.id)];

/** One page of the server's accounts, oldest first. */
export const getAdminUsersQuery = (getPage: Getter<{ limit: number; offset: number }>) =>
	createQuery(() => {
		const { limit, offset } = getPage();
		return {
			queryKey: AuthQueryKeyFactory.adminUsersPage(authStore.user?.id, limit, offset),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(AUTH_ENDPOINTS.adminUsers(limit, offset), { signal }),
			placeholderData: keepPreviousData
		};
	});

export const createSetUserRoleMutation = () =>
	createAppMutation(() => ({
		mutationFn: ({ userId, role }: { userId: string; role: RoleBody['role'] }) =>
			api.v3.PUT(AUTH_ENDPOINTS.adminUserRole(userId), { role }),
		effects: refreshUsers
	}));

/** Creates a local account; it always signs in with a username and password. */
export const createAdminUserMutation = () =>
	createAppMutation(() => ({
		mutationFn: (body: CreateUserBody) => api.v3.POST(AUTH_ENDPOINTS.adminCreateUser, body),
		effects: refreshUsers
	}));

export const createDeleteUserMutation = () =>
	createAppMutation(() => ({
		mutationFn: (userId: string) => api.v3.DELETE(AUTH_ENDPOINTS.adminUser(userId)),
		effects: refreshUsers
	}));

/** Signs one account out of every browser and device. App passwords keep working. */
export const createRevokeUserSessionsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (userId: string) => api.v3.DELETE(AUTH_ENDPOINTS.adminUserSessions(userId)),
		// The user list does not show sessions.
		effects: () => []
	}));
