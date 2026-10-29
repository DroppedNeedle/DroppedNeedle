<script lang="ts">
	import { getDownloadActivitySummaryQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
	import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
	import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import {
		createDownloadActivityObservation,
		reconcileDownloadActivity
	} from '$lib/queries/downloads/downloadActivitySync';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { everyScope } from '$lib/queries/keys';

	const query = getDownloadActivitySummaryQuery();
	const count = $derived(query.data?.active_count ?? 0);

	const observed = createDownloadActivityObservation();

	$effect(() => {
		const userId = authStore.user?.id ?? null;
		const actions = reconcileDownloadActivity(observed, userId, query.data);
		for (const mbid of actions.landedMbids) libraryStore.addMbid(mbid);

		if (actions.refreshDetails && userId) {
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.tasks(userId),
				exact: true
			});
			void invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.heldPrefix(userId)
			});
			if (actions.invalidateLibrary) {
				void invalidateQueriesWithPersister(everyScope(LibraryQueryKeyFactory.feature));
			}
		}
	});
</script>

{#if count > 0}
	<span
		class="absolute -top-1.5 -right-2 badge badge-primary badge-xs h-4 min-w-4 animate-pulse px-1"
		aria-label="{count} active downloads"
	>
		{count}
	</span>
{/if}
