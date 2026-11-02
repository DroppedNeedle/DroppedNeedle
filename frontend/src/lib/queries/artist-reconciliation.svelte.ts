import type { components } from '$lib/api/v3/openapi';
import { key, userScope } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { artistDuplicatesChanged, createAppMutation } from '$lib/queries/effects';

type Schemas = components['schemas'];

export type ArtistReconciliationGroupState = Schemas['ArtistGroupState'];
export type ArtistReconciliationProgress = Schemas['ArtistReconciliationProgress'];
export type ArtistReconciliationMember = Schemas['ArtistReconciliationMember'];
export type ArtistDuplicateGroupSummary = Schemas['ArtistDuplicateGroupSummary'];
export type ArtistDuplicateGroupListResponse = Schemas['ArtistDuplicateGroupListResponse'];
export type ArtistCreditEvidence = Schemas['ArtistCreditEvidence'];
export type ArtistOwnedReference = Schemas['ArtistOwnedReference'];
export type ArtistDuplicateGroupDetail = Schemas['ArtistDuplicateGroupDetail'];
export type ArtistDuplicateGroupDismissResponse = Schemas['ArtistDuplicateGroupDismissResponse'];

export interface ArtistDuplicateGroupParams {
	state?: ArtistReconciliationGroupState;
	search?: string;
}

const normalizedParams = (params: ArtistDuplicateGroupParams) => ({
	state: params.state ?? null,
	search: params.search?.trim() || null
});

type UserId = string | null | undefined;

export const ArtistReconciliationQueryKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'library', 'artist-reconciliation'),
	progress: (userId: UserId) =>
		[...ArtistReconciliationQueryKeyFactory.user(userId), 'progress'] as const,
	groupsPrefix: (userId: UserId) =>
		[...ArtistReconciliationQueryKeyFactory.user(userId), 'groups'] as const,
	groups: (userId: UserId, params: ArtistDuplicateGroupParams) =>
		[
			...ArtistReconciliationQueryKeyFactory.groupsPrefix(userId),
			'list',
			normalizedParams(params)
		] as const,
	group: (userId: UserId, groupId: string) =>
		[...ArtistReconciliationQueryKeyFactory.groupsPrefix(userId), 'detail', groupId] as const
};

const GROUP_PAGE_SIZE = 24;

export const getArtistReconciliationProgressQuery = (getEnabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: getEnabled(),
		queryKey: ArtistReconciliationQueryKeyFactory.progress(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/library/artists/reconciliation'), { signal })
	}));

export const getArtistDuplicateGroupsQuery = (getParams: Getter<ArtistDuplicateGroupParams>) =>
	createInfiniteQuery(() => {
		const params = getParams();
		return {
			queryKey: ArtistReconciliationQueryKeyFactory.groups(authStore.user?.id, params),
			initialPageParam: undefined as string | undefined,
			queryFn: ({ pageParam, signal }) =>
				api.v3.GET(
					v3('/api/v3/library/artists/duplicate-groups', {
						query: {
							limit: GROUP_PAGE_SIZE,
							cursor: pageParam,
							state: params.state,
							search: params.search?.trim() || undefined
						}
					}),
					{ signal }
				),
			getNextPageParam: (lastPage: ArtistDuplicateGroupListResponse) =>
				lastPage.next_cursor ?? undefined
		};
	});

export const getArtistDuplicateGroupQuery = (getGroupId: Getter<string | null>) =>
	createQuery(() => {
		const groupId = getGroupId();
		return {
			enabled: Boolean(groupId),
			queryKey: ArtistReconciliationQueryKeyFactory.group(authStore.user?.id, groupId ?? ''),
			queryFn: ({ signal }) =>
				api.v3.GET(
					v3('/api/v3/library/artists/duplicate-groups/{group_id}', {
						path: { group_id: groupId ?? '' }
					}),
					{ signal }
				)
		};
	});

export const dismissArtistDuplicateGroup = () =>
	createAppMutation(() => ({
		mutationFn: (input: { groupId: string; expectedMemberRevisions: Record<string, number> }) =>
			api.v3.POST(
				v3('/api/v3/library/artists/duplicate-groups/{group_id}/dismiss', {
					path: { group_id: input.groupId }
				}),
				{
					expected_member_revisions: input.expectedMemberRevisions
				}
			),
		effects: () => [artistDuplicatesChanged()],
		onSuccess: () => {
			toastStore.show({ message: 'Artist records marked as distinct', type: 'success' });
		},
		onError: () =>
			toastStore.show({
				message: 'The artist records changed; review the group again',
				type: 'error'
			})
	}));
