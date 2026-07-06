import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { ScrobblePreferencesQueryKeyFactory } from './ScrobblePreferencesQueryKeyFactory';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from './endpoints';
import type { PersonalMixApprovalsResponse } from './types';

type Getter<T> = () => T;

export const getPersonalMixApprovalsQuery = (getEnabled: Getter<boolean>) =>
	createQuery(() => ({
		queryKey: ScrobblePreferencesQueryKeyFactory.personalMixApprovals(),
		queryFn: ({ signal }) =>
			api.global.get<PersonalMixApprovalsResponse>(
				SCROBBLE_PREFERENCES_ENDPOINTS.personalMixApprovals(),
				{ signal }
			),
		enabled: getEnabled()
	}));
