<script lang="ts">
	import { getDownloadActivitySummaryQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
	import {
		createDownloadActivityObservation,
		reconcileDownloadActivity,
		refreshAfterDownloadActivity
	} from '$lib/queries/downloads/downloadActivitySync';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { libraryStore } from '$lib/stores/library';

	const query = getDownloadActivitySummaryQuery();
	const count = $derived(query.data?.active_count ?? 0);

	const observed = createDownloadActivityObservation();

	$effect(() => {
		const userId = authStore.user?.id ?? null;
		const actions = reconcileDownloadActivity(observed, userId, query.data);
		for (const mbid of actions.landedMbids) libraryStore.addMbid(mbid);

		if (actions.refreshDetails && userId) void refreshAfterDownloadActivity(userId, actions);
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
