// The site's CSS section and one person's custom CSS summary, for admins.
import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { AppearanceQueryKeyFactory } from '$lib/queries/featureKeys';
import {
	createAppMutation,
	siteAppearanceChanged,
	userCustomCssChanged
} from '$lib/queries/effects';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';

type Schemas = components['schemas'];
export type SiteAppearanceSettings = Schemas['Appearance'];
export type CustomCssSummary = Schemas['CustomCssSummary'];

export const getSiteAppearanceQuery = () =>
	createQuery(() => ({
		queryKey: AppearanceQueryKeyFactory.site(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/appearance'), { signal })
	}));

export const createSaveSiteAppearanceMutation = () =>
	createAppMutation(() => ({
		mutationFn: (settings: SiteAppearanceSettings) =>
			api.v3.PUT(v3('/api/v3/settings/appearance'), settings),
		onSuccess: (saved) => setQueryDataWithPersister(AppearanceQueryKeyFactory.site(), saved),
		effects: () => [siteAppearanceChanged()]
	}));

export const getUserCustomCssQuery = (accountId: () => string) =>
	createQuery(() => ({
		queryKey: AppearanceQueryKeyFactory.userCustomCss(accountId()),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/admin/users/{id}/custom-css', { path: { id: accountId() } }), {
				signal
			})
	}));

export const createTurnOffUserCustomCssMutation = () =>
	createAppMutation(() => ({
		mutationFn: (accountId: string) =>
			api.v3.POST(v3('/api/v3/admin/users/{id}/custom-css/turn-off', { path: { id: accountId } })),
		onSuccess: (summary, accountId) =>
			setQueryDataWithPersister(AppearanceQueryKeyFactory.userCustomCss(accountId), summary),
		effects: (_summary, accountId) => [userCustomCssChanged(accountId)]
	}));
