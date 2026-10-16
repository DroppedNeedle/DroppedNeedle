<!--
	One library's card on the Automation tab of Library Management: whether
	DroppedNeedle can add a file there only when its name is free. Says
	nothing for a library that can, warns for one whose disk cannot, and
	offers the check for one that has not been checked. Showing this never
	runs the check.
-->
<script lang="ts">
	import { TriangleAlert } from 'lucide-svelte';

	import { checkLibraryManagementPlacementMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { getLibraryManagementPlacementQuery } from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	interface Props {
		rootId: string;
		rootLabel: string;
	}

	let { rootId, rootLabel }: Props = $props();

	const placementQuery = getLibraryManagementPlacementQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const check = checkLibraryManagementPlacementMutation(() => authStore.user?.id);
	const checking = $derived(check.isPending && check.variables === rootId);
	const checkNote =
		'Check again creates a small test folder at the top of this library and removes it again.';
</script>

<QueryBoundary query={placementQuery} what="the overwrite check for this library">
	{#snippet loading()}
		<div class="skeleton mt-3 h-4 w-40" aria-hidden="true"></div>
	{/snippet}
	{#snippet failed(_error, retry)}
		<p class="mt-3 text-xs text-base-content/55">
			Could not load the overwrite check for this library.
			<button type="button" class="link" onclick={retry}>Try again</button>
		</p>
	{/snippet}
	{#snippet children(placement)}
		{@const mode = placement.roots.find((root) => root.root_id === rootId)?.mode}
		{#if mode === 'check'}
			<div class="alert alert-warning mt-3 items-start" role="status">
				<TriangleAlert class="mt-0.5 h-5 w-5" aria-hidden="true" />
				<div class="min-w-0 flex-1 space-y-1 text-sm">
					<strong>Files can be overwritten in this library</strong>
					<p>
						The disk under "{rootLabel}" has no way to add a file only when its name is free.
						DroppedNeedle checks that the name is free and then moves the file in. If another
						program creates a file with that name in the moment between, that file is replaced.
					</p>
					<p>
						Keep other programs from writing to this library while DroppedNeedle manages it, or move
						it to a disk that supports hard links, such as ext4 or an NFS share.
					</p>
					<p class="text-xs">{checkNote}</p>
				</div>
				<AsyncButton size="sm" pending={checking} onclick={() => check.mutate(rootId)}
					>Check again</AsyncButton
				>
			</div>
		{:else if mode === 'unknown'}
			<p class="mt-3 flex flex-wrap items-center gap-2 text-xs text-base-content/60">
				<span>Overwrite check: Not checked yet</span>
				<AsyncButton
					variant="ghost"
					size="xs"
					pending={checking}
					onclick={() => check.mutate(rootId)}>Check again</AsyncButton
				>
				<span class="basis-full">{checkNote}</span>
			</p>
		{/if}
	{/snippet}
</QueryBoundary>
