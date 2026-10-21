<!--
	The files one plugin added to the library, newest first, each with an
	Undo that removes the file while it still holds what the plugin wrote.
-->
<script lang="ts">
	import { Undo2 } from 'lucide-svelte';
	import { getPluginFilesQuery } from '$lib/queries/plugins/PluginQueries.svelte';
	import { undoPluginFileMutation } from '$lib/queries/plugins/PluginMutations.svelte';
	import type { PluginFileView, PluginFilesPage } from '$lib/queries/plugins/types';
	import { formatBytes, formatRelativeTime } from '$lib/utils/formatting';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	interface Props {
		pluginName: string;
	}

	let { pluginName }: Props = $props();

	const filesQuery = getPluginFilesQuery(() => pluginName);
	const undo = undoPluginFileMutation();

	let undoing = $state<PluginFileView | null>(null);
	let confirmOpen = $state(false);
	const undoMessage = $derived(
		`Deletes ${undoing?.relative_path ?? 'the file'} from your library, unless it changed since the plugin wrote it.`
	);

	function noFiles(data: { pages: PluginFilesPage[] }): boolean {
		return data.pages.every((page) => page.items.length === 0);
	}

	function askUndo(file: PluginFileView) {
		undoing = file;
		confirmOpen = true;
	}

	async function confirmUndo() {
		if (!undoing) return;
		await undo.mutateAsync({ name: pluginName, bundleId: undoing.bundle_id });
	}
</script>

<QueryBoundary query={filesQuery} what="this plugin's files" isEmpty={noFiles}>
	{#snippet loading()}
		<div class="skeleton h-10 w-full rounded-lg"></div>
	{/snippet}
	{#snippet empty()}
		<p class="text-xs text-base-content/55">No files yet.</p>
	{/snippet}
	{#snippet children(data)}
		<ul class="divide-y divide-base-content/10">
			{#each data.pages.flatMap((page) => page.items) as file (file.bundle_id)}
				<li class="flex items-center gap-3 py-2">
					<div class="min-w-0 flex-1">
						<p
							class="font-mono text-xs break-all {file.state === 'taken_back'
								? 'text-base-content/45 line-through'
								: ''}"
						>
							{file.relative_path}
						</p>
						<p class="mt-0.5 text-xs text-base-content/55">
							{formatRelativeTime(new Date(file.added_at))} · {formatBytes(file.size)}
						</p>
					</div>
					{#if file.state === 'taken_back'}
						<span class="shrink-0 px-2 text-xs text-base-content/50 italic">Undone</span>
					{:else}
						<Button
							variant="ghost"
							size="xs"
							icon={Undo2}
							aria-label="Undo {file.relative_path}"
							onclick={() => askUndo(file)}
						>
							Undo
						</Button>
					{/if}
				</li>
			{/each}
		</ul>
		{#if filesQuery.hasNextPage}
			<div class="flex justify-center pt-2">
				<Button
					variant="ghost"
					size="xs"
					onclick={() => void filesQuery.fetchNextPage()}
					disabled={filesQuery.isFetchingNextPage}
				>
					Show more
				</Button>
			</div>
		{/if}
	{/snippet}
</QueryBoundary>

<ConfirmDialog
	bind:open={confirmOpen}
	title="Undo this file?"
	message={undoMessage}
	confirmLabel="Undo"
	tone="danger"
	onconfirm={confirmUndo}
/>
