import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import {
	invalidateQueriesWithPersister,
	queryClient,
	setQueryDataWithPersister
} from '../QueryClient';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { CONCERT_ENDPOINTS, FOLLOW_ENDPOINTS } from './endpoints';
import { toFollowStatus } from './FollowAdapters';
import type {
	AutoDownloadState,
	EventCitiesResponse,
	EventCity,
	FollowStatus,
	UnseenCountResponse
} from './types';

const NOT_FOLLOWING: FollowStatus = {
	followed: false,
	auto_download: false,
	auto_download_state: 'none'
};

function statusKey(mbid: string) {
	return FollowQueryKeyFactory.status(mbid, authStore.user?.id);
}

function invalidateFollowedArtists(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: FollowQueryKeyFactory.artists(authStore.user?.id)
	});
}

export const createSetFollowMutation = (getMbid: () => string) =>
	createMutation(() => ({
		mutationFn: async (followed: boolean) =>
			toFollowStatus(await api.global.v3.PUT(FOLLOW_ENDPOINTS.setFollow(getMbid()), { followed })),
		onMutate: async (followed: boolean) => {
			const key = statusKey(getMbid());
			await queryClient.cancelQueries({ queryKey: key });
			const prev = queryClient.getQueryData<FollowStatus>(key) ?? NOT_FOLLOWING;
			// unfollowing also clears the auto-download intent
			const optimistic: FollowStatus = followed
				? { ...prev, followed: true }
				: { ...NOT_FOLLOWING };
			await setQueryDataWithPersister<FollowStatus>(key, optimistic);
			return { prev };
		},
		onError: (_err, _followed, ctx) => {
			if (ctx) void setQueryDataWithPersister<FollowStatus>(statusKey(getMbid()), ctx.prev);
		},
		onSuccess: (data) => {
			void setQueryDataWithPersister<FollowStatus>(statusKey(getMbid()), data);
		},
		onSettled: () => {
			void invalidateFollowedArtists();
		}
	}));

// fired on New Releases page mount; writes count 0 into the persisted cache
// so the badge clears without waiting for a refetch
export const createMarkNewReleasesSeenMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(FOLLOW_ENDPOINTS.markNewReleasesSeen()),
		onSuccess: (data) => {
			void setQueryDataWithPersister<UnseenCountResponse>(
				FollowQueryKeyFactory.newReleasesUnseen(authStore.user?.id),
				data
			);
		}
	}));

// replace-all semantics: the city manager submits its full list in order
export const createReplaceEventCitiesMutation = () =>
	createMutation(() => ({
		mutationFn: (cities: EventCity[]) =>
			api.global.v3.PUT(CONCERT_ENDPOINTS.concertCities(), { items: cities }),
		onSuccess: (data) => {
			void setQueryDataWithPersister<EventCitiesResponse>(
				FollowQueryKeyFactory.concertCities(authStore.user?.id),
				data
			);
			// the city set changes what the concerts list and badge count contain
			void invalidateQueriesWithPersister({
				queryKey: FollowQueryKeyFactory.concerts(authStore.user?.id)
			});
			void invalidateQueriesWithPersister({
				queryKey: FollowQueryKeyFactory.concertsUnseen(authStore.user?.id)
			});
		},
		onError: () => {
			toastStore.show({ message: "Couldn't save your cities.", type: 'error' });
		}
	}));

// fired on Upcoming Events page mount; writes count 0 into the persisted
// cache so the badge clears without waiting for a refetch
export const createMarkConcertsSeenMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.v3.POST(CONCERT_ENDPOINTS.markConcertsSeen()),
		onSuccess: (data) => {
			void setQueryDataWithPersister<UnseenCountResponse>(
				FollowQueryKeyFactory.concertsUnseen(authStore.user?.id),
				data
			);
		}
	}));

// mbid is the mutation variable so one instance serves every card in the hub
export const createUnfollowMutation = () =>
	createMutation(() => ({
		mutationFn: async (mbid: string) =>
			toFollowStatus(
				await api.global.v3.PUT(FOLLOW_ENDPOINTS.setFollow(mbid), { followed: false })
			),
		onSuccess: (data, mbid) => {
			void setQueryDataWithPersister<FollowStatus>(statusKey(mbid), data);
		},
		onSettled: () => {
			void invalidateFollowedArtists();
		}
	}));

export const createSetAutoDownloadMutation = (getMbid: () => string) =>
	createMutation(() => ({
		mutationFn: async (enabled: boolean) =>
			toFollowStatus(
				await api.global.v3.PUT(FOLLOW_ENDPOINTS.autoDownload(getMbid()), { enabled })
			),
		onMutate: async (enabled: boolean) => {
			const key = statusKey(getMbid());
			await queryClient.cancelQueries({ queryKey: key });
			const prev = queryClient.getQueryData<FollowStatus>(key) ?? NOT_FOLLOWING;
			// admins are approved immediately; everyone else enters a pending state (D3)
			const optimisticState: AutoDownloadState = enabled
				? authStore.isAdmin
					? 'approved'
					: 'pending'
				: 'none';
			await setQueryDataWithPersister<FollowStatus>(key, {
				...prev,
				auto_download: enabled,
				auto_download_state: optimisticState
			});
			return { prev };
		},
		onError: (_err, _enabled, ctx) => {
			if (ctx) void setQueryDataWithPersister<FollowStatus>(statusKey(getMbid()), ctx.prev);
		},
		onSuccess: (data, enabled) => {
			void setQueryDataWithPersister<FollowStatus>(statusKey(getMbid()), data);
			if (enabled && data.auto_download_state === 'pending') {
				toastStore.show({
					message: 'Request sent - an admin will approve auto-downloads for this artist.',
					type: 'info'
				});
			}
		},
		onSettled: () => {
			void invalidateFollowedArtists();
		}
	}));
