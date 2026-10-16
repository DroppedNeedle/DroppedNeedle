<script lang="ts">
	// separate component, not layout-script state: createQuery needs the
	// QueryProvider context, which the layout itself renders. Sits as the
	// right-hand circle of the overlapping badge pair on the Following link
	// (left = new releases, right = concerts).
	import { getUnseenConcertsCountQuery } from '$lib/queries/following/FollowQueries.svelte';

	const query = getUnseenConcertsCountQuery();
	const count = $derived(query.data?.count ?? 0);
</script>

{#if count > 0}
	<span
		class="absolute -top-1.5 -right-2 badge badge-secondary badge-xs h-4 min-w-4 translate-x-[85%] px-1"
		aria-label="{count} upcoming concerts from followed artists"
	>
		{count > 9 ? '9+' : count}
	</span>
{/if}
