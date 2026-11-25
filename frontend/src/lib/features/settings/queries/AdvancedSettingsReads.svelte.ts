import { v3 } from '$lib/api/v3/endpoint';
// App-wide reads of the advanced settings section. Only an admin may read the
// section; everyone else, and anyone before the first answer, gets the defaults.
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	AdvancedSettingsQueryKeyFactory,
	type AdvancedSettings
} from './SectionSettingsQueries.svelte';

const advancedSettingsQueryOptions = () =>
	queryOptions({
		queryKey: AdvancedSettingsQueryKeyFactory.saved(authStore.user?.id),
		queryFn: ({ signal }): Promise<AdvancedSettings> =>
			api.v3.GET(v3('/api/v3/settings/advanced'), { signal }),
		enabled: authStore.isAdmin
	});

/** Loads the section for an admin; the app shell holds one for the session. */
export const loadAdvancedSettings = () =>
	createQuery(advancedSettingsQueryOptions, () => queryClient);

/** Whether images may load straight from their outside host. Read by every image. */
export function useDirectRemoteImages() {
	const query = createQuery(
		() => ({ ...advancedSettingsQueryOptions(), enabled: false }),
		() => queryClient
	);
	const enabled = $derived(query.data?.direct_remote_images_enabled ?? true);
	return {
		get current() {
			return enabled;
		}
	};
}
