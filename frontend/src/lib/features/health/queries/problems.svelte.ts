import { createQuery } from '@tanstack/svelte-query';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { ProblemsQueryKeyFactory } from '$lib/queries/featureKeys';

export type ProblemsResponse = components['schemas']['ProblemsResponse'];
export type Problem = components['schemas']['Problem'];
export type ProblemKind = components['schemas']['ProblemKind'];
export type ProblemSeverity = components['schemas']['ProblemSeverity'];

/** What is wrong now. Admin only; looked at again every minute and on focus. */
export const getAdminProblemsQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: ProblemsQueryKeyFactory.list(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/admin/problems'), { signal }),
		refetchInterval: 60_000,
		refetchOnWindowFocus: true
	}));
