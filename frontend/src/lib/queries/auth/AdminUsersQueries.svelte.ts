import { createMutation, createQuery, keepPreviousData } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { V3Body } from '$lib/api/v3/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { userIdSegment } from '../userKeySegment';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';

export type AdminUser = components['schemas']['UserResponse'];
type CreateUserBody = V3Body<'/api/v3/admin/users', 'post'>;
type RoleBody = V3Body<'/api/v3/admin/users/{id}/role', 'put'>;

// Under the auth prefix so a sign-out clears it; keyed by the admin reading it.
export const AdminUsersQueryKeyFactory = {
	all: (userId: string | null | undefined) =>
		[...AuthQueryKeyFactory.prefix, 'admin-users', userIdSegment(userId)] as const,
	page: (userId: string | null | undefined, limit: number, offset: number) =>
		[...AdminUsersQueryKeyFactory.all(userId), { limit, offset }] as const
};

const refreshUsers = () =>
	invalidateQueriesWithPersister({ queryKey: AdminUsersQueryKeyFactory.all(authStore.user?.id) });

/** One page of the server's accounts, oldest first. */
export const getAdminUsersQuery = (getPage: Getter<{ limit: number; offset: number }>) =>
	createQuery(() => {
		const { limit, offset } = getPage();
		return {
			queryKey: AdminUsersQueryKeyFactory.page(authStore.user?.id, limit, offset),
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
