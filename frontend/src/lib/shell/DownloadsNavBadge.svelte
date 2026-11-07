<script lang="ts">
	import { getDownloadActivitySummaryQuery } from '$lib/features/downloads/queries/DownloadQueries.svelte';
	import {
		createDownloadActivityObservation,
		reconcileDownloadActivity,
		refreshAfterDownloadActivity
	} from '$lib/features/downloads/queries/downloadActivitySync';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { libraryStore } from '$lib/stores/library';

	const query = getDownloadActivitySummaryQuery();
	const count = $derived(query.data?.active_count ?? 0);
	// Shown only while nothing is active: one number, one badge.
	const held = $derived(query.data?.held_count ?? 0);

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
{:else if held > 0}
	<span
		class="absolute -top-1.5 -right-2 badge badge-warning badge-xs h-4 min-w-4 px-1"
		aria-label="{held} held for you to check"
	>
		{held}
	</span>
{/if}
