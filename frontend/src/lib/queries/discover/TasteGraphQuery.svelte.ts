import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { createQuery } from '@tanstack/svelte-query';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export interface TasteGraphReason {
	type: 'collaborator' | 'member' | 'label' | 'scene';
	label: string;
	via_mbid?: string | null;
	via_name?: string | null;
}

export interface TasteGraphSeed {
	artist_mbid: string;
	name: string;
	weight: number;
}

export interface TasteGraphItem {
	kind: 'artist' | 'album';
	mbid: string;
	name: string;
	artist_mbid?: string | null;
	artist_name?: string | null;
	score: number;
	reasons: TasteGraphReason[];
	in_library: boolean;
}

export interface TasteGraphResponse {
	cold_start: boolean;
	generated_at: string;
	seeds: TasteGraphSeed[];
	items: TasteGraphItem[];
}

// The graph is rebuilt server-side on library changes at a slow cadence, so a
// long client staleTime is fine. retry: false — old backends lack the route and
// a 404 should settle into the quiet invitation state immediately. Focus refetch
// is off: an errored query is permanently stale, so a missing route would
// otherwise re-404 on every tab focus.
const TASTE_GRAPH_STALE_MS = 24 * 60 * 60 * 1000;

export const getTasteGraphQuery = () =>
	createQuery(() => ({
		staleTime: TASTE_GRAPH_STALE_MS,
		retry: false,
		refetchOnWindowFocus: false,
		queryKey: [
			...DiscoverQueryKeyFactory.prefix,
			authStore.user?.id ?? null,
			'taste-graph'
		] as const,
		queryFn: ({ signal }) =>
			api.global.get<TasteGraphResponse>(API.discoverTasteGraph(), { signal })
	}));
