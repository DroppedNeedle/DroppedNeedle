// App-wide reads of the advanced settings section. Only an admin may read the
// section; everyone else, and anyone before the first answer, gets the defaults.
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { SETTINGS_ENDPOINTS } from './endpoints';
import {
	AdvancedSettingsQueryKeyFactory,
	type AdvancedSettings
} from './SectionSettingsQueries.svelte';

const advancedSettingsQueryOptions = () =>
	queryOptions({
		queryKey: AdvancedSettingsQueryKeyFactory.saved(authStore.user?.id),
		queryFn: ({ signal }): Promise<AdvancedSettings> =>
			api.global.v3.GET(SETTINGS_ENDPOINTS.advanced(), { signal }),
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

/** Whether the Discover queue builds a new deck by itself. */
export async function discoverQueueAutoGenerate(): Promise<boolean> {
	if (!authStore.isAdmin) return true;
	try {
		const settings = await queryClient.ensureQueryData(advancedSettingsQueryOptions());
		return settings.discover_queue_auto_generate ?? true;
	} catch {
		return true;
	}
}
