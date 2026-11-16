import { v3 } from '$lib/api/v3/endpoint';
import { createInfiniteQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory } from '$lib/features/library/queries/LibraryQueryKeyFactory';
import { authStore } from '$lib/stores/authStore.svelte';

export type DuplicateGroup = components['schemas']['DuplicateGroupView'];
export type DuplicateCopy = components['schemas']['DuplicateCopyView'];
export type DuplicateClass = components['schemas']['DuplicateClass'];
export type DuplicateEvidence = components['schemas']['DuplicateEvidence'];
export type DuplicateDecisionSource = components['schemas']['DecisionSource'];
export type DuplicateGroupPage = components['schemas']['Page_DuplicateGroupView'];

const PAGE_SIZE = 24;

// Albums the library lists more than once, a page of groups at a time.
// Read-only: the page links to each copy and offers no action.
export const getDuplicateAlbumsQuery = () =>
	createInfiniteQuery(() => ({
		queryKey: LibraryQueryKeyFactory.catalog.duplicates(authStore.user?.id),
		initialPageParam: null as string | null,
		queryFn: ({ pageParam, signal }) =>
			api.v3.GET(
				v3('/api/v3/library/duplicates', {
					query: { limit: PAGE_SIZE, ...(pageParam ? { cursor: pageParam } : {}) }
				}),
				{ signal }
			),
		getNextPageParam: (lastPage: DuplicateGroupPage) => lastPage.next_cursor ?? undefined
	}));
