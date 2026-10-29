import { createMutation, createQuery, keepPreviousData } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { V3Body } from '$lib/api/v3/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

export type AdminUser = components['schemas']['UserResponse'];
type CreateUserBody = V3Body<'/api/v3/admin/users', 'post'>;
type RoleBody = V3Body<'/api/v3/admin/users/{id}/role', 'put'>;

const refreshUsers = () =>
	invalidateQueriesWithPersister({ queryKey: AuthQueryKeyFactory.adminUsers(authStore.user?.id) });

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
	createMutation(() => ({
		mutationFn: ({ userId, role }: { userId: string; role: RoleBody['role'] }) =>
			api.v3.PUT(AUTH_ENDPOINTS.adminUserRole(userId), { role }),
		onSuccess: refreshUsers
	}));

/** Creates a local account; it always signs in with a username and password. */
export const createAdminUserMutation = () =>
	createMutation(() => ({
		mutationFn: (body: CreateUserBody) => api.v3.POST(AUTH_ENDPOINTS.adminCreateUser, body),
		onSuccess: refreshUsers
	}));

export const createDeleteUserMutation = () =>
	createMutation(() => ({
		mutationFn: (userId: string) => api.v3.DELETE(AUTH_ENDPOINTS.adminUser(userId)),
		onSuccess: refreshUsers
	}));
