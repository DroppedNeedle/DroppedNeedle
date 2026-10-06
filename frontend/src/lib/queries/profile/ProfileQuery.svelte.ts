import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { ProfileQueryKeyFactory } from './ProfileQueryKeyFactory';
import { PROFILE_ENDPOINTS } from './endpoints';

/** The current user's profile (identity only; see `ProfileData`). Keyed by
 *  `userId` so the cache never leaks across users on a shared browser
 *  (AMU-5). */
export const getProfileQuery = (getUserId: Getter<string>) =>
	createQuery(() => ({
		queryKey: ProfileQueryKeyFactory.profile(getUserId()),
		queryFn: ({ signal }) => api.global.v3.GET(PROFILE_ENDPOINTS.get(), { signal })
	}));
