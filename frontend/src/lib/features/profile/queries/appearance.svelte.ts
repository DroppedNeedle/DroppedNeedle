import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { AppearanceQueryKeyFactory } from '$lib/queries/featureKeys';
import { createAppMutation, myAppearanceChanged } from '$lib/queries/effects';
import { PERSIST } from '$lib/queries/persistedCache';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';

type Schemas = components['schemas'];
export type MyAppearance = Schemas['MyAppearance'];
export type AppearanceUpdate = Schemas['AppearanceUpdate'];
export type AppearanceImported = Schemas['AppearanceImported'];
export type CssSave = Schemas['CssSave'];
export type CssProblem = Schemas['CssProblem'];
export type ThemeChoice = Schemas['ThemeChoice'];

/** A GET answer that came from the server, not from the browser's saved copy. */
export interface FetchedAppearance {
	userId: string;
	answer: MyAppearance;
	fetchedAt: number;
}

let lastFetched: FetchedAppearance | null = null;

/**
 * The newest appearance the server answered on this page, once: taking it
 * clears it. A copy restored from the browser never runs the query function,
 * so it never lands here.
 */
export function takeFetchedAppearance(): FetchedAppearance | null {
	const fetched = lastFetched;
	lastFetched = null;
	return fetched;
}

// Saved in the browser, so a reload paints the account's sheet and theme at once.
export const getMyAppearanceQuery = () =>
	createQuery(() => {
		const userId = authStore.user?.id;
		return {
			queryKey: AppearanceQueryKeyFactory.mine(userId),
			queryFn: async ({ signal }) => {
				const answer = await api.v3.GET(v3('/api/v3/me/appearance'), { signal });
				if (userId) lastFetched = { userId, answer, fetchedAt: Date.now() };
				return answer;
			},
			enabled: Boolean(userId),
			meta: PERSIST
		};
	});

export const getMyCssSavesQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({
		queryKey: AppearanceQueryKeyFactory.cssSaves(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/me/appearance/css-saves'), { signal }),
		enabled: Boolean(authStore.user?.id) && enabled()
	}));

async function keepAnswer(userId: string | undefined, answer: MyAppearance): Promise<void> {
	if (!userId || authStore.user?.id !== userId) return;
	const { theme, custom_css, site, stored } = answer;
	await setQueryDataWithPersister(AppearanceQueryKeyFactory.mine(userId), {
		theme,
		custom_css,
		site,
		stored
	});
}

/** Save the caller's theme or CSS. Send only the fields that changed. */
export const createSaveAppearanceMutation = () =>
	createAppMutation(() => ({
		mutationFn: (update: AppearanceUpdate) => api.v3.PUT(v3('/api/v3/me/appearance'), update),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: (answer, _update, context) => keepAnswer(context.userId, answer),
		effects: (_answer, _update, context) => [myAppearanceChanged(context?.userId)]
	}));

/** Move what this browser kept to the account, once; an account that holds a row keeps it. */
export const createImportAppearanceMutation = () =>
	createAppMutation(() => ({
		mutationFn: (update: AppearanceUpdate) =>
			api.v3.POST(v3('/api/v3/me/appearance/import'), update),
		onMutate: () => ({ userId: authStore.user?.id }),
		onSuccess: (answer, _update, context) => keepAnswer(context.userId, answer),
		effects: (_answer, _update, context) => [myAppearanceChanged(context?.userId)]
	}));
