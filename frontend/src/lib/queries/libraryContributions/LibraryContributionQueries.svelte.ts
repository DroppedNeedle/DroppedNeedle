import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryContributionApi } from './LibraryContributionApi';
import { authStore } from '$lib/stores/authStore.svelte';
import type { LibraryContribution } from '$lib/types';
import { LibraryContributionQueryKeyFactory } from './LibraryContributionQueryKeyFactory';

export const getLibraryContributionQuery = (getContributionId: Getter<string>) => {
	const query = createQuery(() => {
		const contributionId = getContributionId();
		return {
			enabled: Boolean(authStore.user?.id && contributionId),
			queryKey: LibraryContributionQueryKeyFactory.detail(authStore.user?.id, contributionId),
			queryFn: async ({ signal }) => {
				const contribution = await api.global.get<LibraryContribution>(
					LibraryContributionApi.detail(contributionId),
					{
						signal
					}
				);
				return contribution;
			},
			refetchInterval: (query: { state: { data?: LibraryContribution } }) =>
				['seeded', 'verifying'].includes(query.state.data?.state ?? '') ? 2_000 : false
		};
	});
	return query;
};
