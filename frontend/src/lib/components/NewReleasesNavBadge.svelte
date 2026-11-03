<script lang="ts">
	// separate component, not layout-script state: createQuery needs the
	// QueryProvider context, which the layout itself renders
	import { getUnseenNewReleasesCountQuery } from '$lib/features/following/queries/FollowQueries.svelte';

	const query = getUnseenNewReleasesCountQuery();
	const count = $derived(query.data?.count ?? 0);
</script>

{#if count > 0}
	<span
		class="absolute -top-1.5 -right-2 badge badge-primary badge-xs h-4 min-w-4 px-1"
		aria-label="{count} new releases from followed artists"
	>
		{count > 9 ? '9+' : count}
	</span>
{/if}
