import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { CONCERT_ENDPOINTS, FOLLOW_ENDPOINTS } from './endpoints';
import { toFollowStatus, toFollowedArtist, toNewRelease } from './FollowAdapters';
import type { NewReleasesResponse } from './types';

type Getter<T> = () => T;

export const getFollowStatusQuery = (getMbid: Getter<string>) =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.status(getMbid(), authStore.user?.id),
		queryFn: async ({ signal }) =>
			toFollowStatus(await api.global.v3.GET(FOLLOW_ENDPOINTS.status(getMbid()), { signal }))
	}));

export const getFollowedArtistsQuery = () =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.artists(authStore.user?.id),
		queryFn: async ({ signal }) => {
			const data = await api.global.v3.GET(FOLLOW_ENDPOINTS.followedArtists(), { signal });
			return data.artists.map(toFollowedArtist);
		}
	}));

// The release log. v3 serves one recent window without a day range, limit or
// owned flag, so the limit applies here and the window/owned filters stay in
// the key only to keep the page's choices cached apart.
export const getRecentReleasesQuery = (
	getDays: Getter<number>,
	getLimit: Getter<number>,
	getIncludeOwned: Getter<boolean> = () => true
) =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.recentReleases(
			authStore.user?.id,
			getDays(),
			getLimit(),
			getIncludeOwned()
		),
		queryFn: async ({ signal }): Promise<NewReleasesResponse> => {
			const data = await api.global.v3.GET(FOLLOW_ENDPOINTS.recentReleases(), { signal });
			return { items: data.items.slice(0, getLimit()).map(toNewRelease), total: data.total };
		}
	}));

export const getConcertsQuery = () =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.concerts(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(CONCERT_ENDPOINTS.concerts(), { signal })
	}));

export const getEventCitiesQuery = () =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.concertCities(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(CONCERT_ENDPOINTS.concertCities(), { signal })
	}));

// enabled only from 2 chars (the backend's min query length); callers debounce
export const getCitySearchQuery = (getQ: Getter<string>) =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.citySearch(authStore.user?.id, getQ()),
		queryFn: ({ signal }) =>
			api.global.v3.GET(CONCERT_ENDPOINTS.concertCitySearch(getQ()), { signal }),
		enabled: getQ().trim().length >= 2
	}));

// drives the concerts half of the sidebar badge pair (same cadence rationale
// as the new-releases badge below)
export const getUnseenConcertsCountQuery = () =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.concertsUnseen(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(CONCERT_ENDPOINTS.concertsUnseenCount(), { signal }),
		enabled: !!authStore.user?.id,
		refetchInterval: 60_000
	}));

// drives the sidebar badge; the feed only changes when the daily poller runs,
// so a slow interval (plus refetch-on-focus) is plenty
export const getUnseenNewReleasesCountQuery = () =>
	createQuery(() => ({
		queryKey: FollowQueryKeyFactory.newReleasesUnseen(authStore.user?.id),
		queryFn: ({ signal }) =>
			api.global.v3.GET(FOLLOW_ENDPOINTS.newReleasesUnseenCount(), { signal }),
		enabled: !!authStore.user?.id,
		refetchInterval: 60_000
	}));
