<!--
	The last saves of the person's CSS, newest first, each with when it was
	saved and its length. Restore hands its text to `onrestore`, which puts it
	in the editor without saving.
-->
<script lang="ts">
	import { RotateCcw } from 'lucide-svelte';
	import { getMyCssSavesQuery } from '$lib/features/profile/queries/appearance.svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { formatRelativeTime } from '$lib/utils/formatting';

	interface Props {
		onrestore: (css: string) => void;
		disabled?: boolean;
	}

	let { onrestore, disabled = false }: Props = $props();

	const saves = getMyCssSavesQuery();
</script>

<QueryBoundary query={saves} what="your CSS history">
	{#snippet empty()}
		<p class="px-2 py-3 text-sm text-muted">Nothing saved yet.</p>
	{/snippet}
	{#snippet children(list)}
		<ul class="css-rail-list" aria-label="Saved versions">
			{#each list.slice(0, 10) as save (save.id)}
				<li class="css-history-row">
					<span class="min-w-0 flex-1">
						<span class="block text-sm" title={new Date(save.saved_at).toLocaleString()}>
							{formatRelativeTime(new Date(save.saved_at))}
						</span>
						<span class="block font-mono text-xs text-muted">
							{save.css.length.toLocaleString()} characters
						</span>
					</span>
					<Button
						variant="ghost"
						size="sm"
						icon={RotateCcw}
						{disabled}
						aria-label="Restore the save from {new Date(save.saved_at).toLocaleString()}"
						onclick={() => onrestore(save.css)}
					>
						Restore
					</Button>
				</li>
			{/each}
		</ul>
	{/snippet}
</QueryBoundary>
