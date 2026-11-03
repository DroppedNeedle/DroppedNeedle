<!--
	The review list's narrowing controls: folder, policy, identification,
	filing dates, order and the two toggles. Each writes straight into the
	shared filters.
-->
<script lang="ts">
	import Input from '$lib/ui/Input.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { REVIEW_SORTS, type ReviewFilters } from './reviewFilters.svelte';

	interface Props {
		filters: ReviewFilters;
		roots: { id: string; label: string }[];
	}

	let { filters, roots }: Props = $props();
</script>

<div class="flex flex-wrap items-end gap-2 text-sm">
	<label class="grid gap-1 text-xs">
		<span>Order</span>
		<select class="select select-sm" bind:value={filters.sort}>
			{#each REVIEW_SORTS as sort (sort.value)}<option value={sort.value}>{sort.label}</option
				>{/each}
		</select>
	</label>
	{#if authStore.isAdmin && roots.length > 1}
		<label class="grid gap-1 text-xs">
			<span>Folder</span>
			<select class="select select-sm" bind:value={filters.rootId}>
				<option value="">All folders</option>
				{#each roots as root (root.id)}<option value={root.id}>{root.label}</option>{/each}
			</select>
		</label>
	{/if}
	<label class="grid gap-1 text-xs">
		<span>Folder policy</span>
		<select class="select select-sm" bind:value={filters.policy}>
			<option value="">Any</option>
			<option value="automatic">Automatic</option>
			<option value="local_metadata">Local metadata</option>
			<option value="excluded">Excluded</option>
		</select>
	</label>
	<label class="grid gap-1 text-xs">
		<span>Identification</span>
		<select class="select select-sm" bind:value={filters.jobState}>
			<option value="">Any</option>
			<option value="queued">Queued</option>
			<option value="running">Running</option>
			<option value="deferred">Waiting to retry</option>
			<option value="failed">Failed</option>
			<option value="attention">Needs attention</option>
			<option value="succeeded">Finished</option>
		</select>
	</label>
	<label class="grid gap-1 text-xs">
		<span>Filed from</span>
		<Input type="date" size="sm" label="Filed from" bind:value={filters.filedFrom} />
	</label>
	<label class="grid gap-1 text-xs">
		<span>Filed to</span>
		<Input type="date" size="sm" label="Filed to" bind:value={filters.filedTo} />
	</label>
	<label class="label cursor-pointer gap-2 pb-1 text-xs">
		<input type="checkbox" class="checkbox checkbox-sm" bind:checked={filters.withCandidates} />
		Only with candidates
	</label>
	<label class="label cursor-pointer gap-2 pb-1 text-xs">
		<input type="checkbox" class="checkbox checkbox-sm" bind:checked={filters.hideActiveJobs} />
		Hide albums still being matched
	</label>
</div>
