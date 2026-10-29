import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { authStore } from '$lib/stores/authStore.svelte';
import type { SectionPrefsResponse, SectionPrefsUpdate } from '$lib/types';
import { createQuery } from '@tanstack/svelte-query';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { SectionPrefsQueryKeyFactory } from './SectionPrefsQueryKeyFactory';
import { anyDiscoverChanged, applyEffects, homeChanged } from '$lib/queries/effects';

const SECTION_PREFS_ENDPOINT = () => v3('/api/v3/me/section-prefs');

// The contract leaves `pages` and `requires` optional; the page model fills them.
function toResponse(data: { pages?: SectionPrefsResponse['pages'] }): SectionPrefsResponse {
	return { pages: data.pages ?? {} };
}

export const getSectionPrefsQuery = () =>
	createQuery(() => ({
		queryKey: SectionPrefsQueryKeyFactory.prefs(authStore.user?.id),
		queryFn: async ({ signal }) =>
			toResponse(await api.v3.GET(SECTION_PREFS_ENDPOINT(), { signal }))
	}));

/** Save one page's toggles; refreshes the prefs cache and invalidates the page data. */
export async function saveSectionPrefs(update: SectionPrefsUpdate): Promise<void> {
	const saved = toResponse(await api.v3.PUT(SECTION_PREFS_ENDPOINT(), update));
	const userId = authStore.user?.id;
	await setQueryDataWithPersister<SectionPrefsResponse>(
		SectionPrefsQueryKeyFactory.prefs(userId),
		(prev) => ({
			pages: { ...(prev?.pages ?? {}), ...saved.pages }
		})
	);
	// the home/discover responses are filtered server-side, so their caches are stale
	// now; sidebar prefs are client-only chrome read straight from the prefs cache
	if (update.page === 'home') {
		await applyEffects(homeChanged());
	} else if (update.page === 'discover') {
		await applyEffects(anyDiscoverChanged());
	}
}
