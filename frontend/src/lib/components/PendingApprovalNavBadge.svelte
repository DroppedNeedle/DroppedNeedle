<script lang="ts">
	import { getPendingApprovalCountQuery } from '$lib/features/following/queries/AdminApprovalsQueries.svelte';

	const query = getPendingApprovalCountQuery(() => true);
	const count = $derived(query.data?.count ?? 0);
</script>

{#if count > 0}
	<span
		class="absolute -top-2 -right-2 badge badge-warning badge-xs h-4 w-4 p-0 text-[10px] font-bold"
		aria-label="{count} pending approvals"
	>
		{count > 9 ? '9+' : count}
	</span>
{/if}
