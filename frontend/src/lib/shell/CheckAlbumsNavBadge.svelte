<!--
	A quiet count on Check albums: the albums that are a best guess, that
	someone marked "Looks wrong" or that have an open review, each once.
	Nothing is urgent here, so it is drawn in the neutral colour.
-->
<script lang="ts">
	import { getLibraryStatsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';

	const stats = getLibraryStatsQuery();
	const count = $derived(stats.data?.check_count ?? 0);
</script>

{#if count > 0}
	<span
		class="pointer-events-none absolute -top-2 -right-2 badge badge-xs h-4 min-w-4 border-0 bg-base-content/25 px-1 text-[10px] font-semibold text-base-content tabular-nums"
		aria-label="{count} albums to check"
	>
		{count > 99 ? '99+' : count}
	</span>
{/if}
