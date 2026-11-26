import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation, tuningsChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { DiscoverQueryKeyFactory } from './DiscoverQueryKeyFactory';

export type TasteResponse = components['schemas']['DiscoverTasteResponse'];
export type TasteGroup = components['schemas']['TasteGroup'];
export type TasteArtist = components['schemas']['TasteArtist'];
export type TuningKind = components['schemas']['TuningKind'];
export type TuningEntry = components['schemas']['DiscoverTuningEntry'];

/** How often the page reads again while the picks are catching up with a change... */
const UPDATING_MS = 5000;
/** ...for at most this long at a stretch. */
const UPDATING_FOR_MS = 3 * 60 * 1000;

/** The caller's taste: their groups, each with its artists and what they said. */
export const getTasteQuery = () => {
	// When the page began reading again for one stretch of updating.
	let since: number | null = null;
	return createQuery(() => ({
		enabled: Boolean(authStore.user?.id),
		queryKey: DiscoverQueryKeyFactory.v3.taste(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/discover/taste'), { signal }),
		refetchInterval: (query) => {
			if (!query.state.data?.updating) {
				since = null;
				return false;
			}
			since ??= Date.now();
			return Date.now() - since < UPDATING_FOR_MS ? UPDATING_MS : false;
		}
	}));
};

/** One statement: about one of the caller's artists, or every artist of a group. */
export type TasteStatement =
	| { kind: TuningKind; target: string; group_no?: never }
	| { kind: TuningKind; group_no: number; target?: never };

/** "More", "Less" or "That was a phase", for one artist or a whole group. */
export const createTasteStatementMutation = () =>
	createAppMutation(() => ({
		mutationFn: (statement: TasteStatement) =>
			api.v3.POST(v3('/api/v3/discover/tunings'), statement),
		effects: () => [tuningsChanged(authStore.user?.id)],
		onSuccess: (written, statement) => {
			if (statement.group_no !== undefined && written.items.length === 0)
				toastStore.show({
					message: 'Nothing to change: each artist already has its own choice.',
					type: 'info'
				});
		},
		onError: () => toastStore.show({ message: "Couldn't save that.", type: 'error' })
	}));
