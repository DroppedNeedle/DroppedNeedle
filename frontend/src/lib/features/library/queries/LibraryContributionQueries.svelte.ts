import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import type { LibraryContribution } from '$lib/features/library/queries/types';
import { LibraryContributionQueryKeyFactory } from './LibraryContributionQueryKeyFactory';

export const getLibraryContributionQuery = (getContributionId: Getter<string>) => {
	const query = createQuery(() => {
		const contributionId = getContributionId();
		return {
			enabled: Boolean(authStore.user?.id && contributionId),
			queryKey: LibraryContributionQueryKeyFactory.detail(authStore.user?.id, contributionId),
			queryFn: async ({ signal }) => {
				return api.v3.GET(
					v3('/api/v3/library/contributions/{id}', { path: { id: contributionId } }),
					{
						signal
					}
				);
			},
			refetchInterval: (query: { state: { data?: LibraryContribution } }) =>
				['seeded', 'verifying'].includes(query.state.data?.state ?? '') ? 2_000 : false
		};
	});
	return query;
};
