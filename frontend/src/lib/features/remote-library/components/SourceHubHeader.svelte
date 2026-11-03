<script lang="ts">
	import { RefreshCw } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		icon: Snippet;
		title: string;
		albumCount: number | null;
		settingsSnippet?: Snippet;
		onrefresh?: () => void;
		refreshing?: boolean;
	}

	let { icon, title, albumCount, settingsSnippet, onrefresh, refreshing = false }: Props = $props();
</script>

<div class="flex flex-wrap items-center justify-between gap-4 px-1 pb-2 pt-2">
	<div class="flex items-center gap-3">
		<span class="text-primary">
			{@render icon()}
		</span>
		<h1 class="text-2xl font-bold text-base-content sm:text-3xl">{title}</h1>
		{#if albumCount !== null}
			<span class="badge badge-ghost badge-sm font-mono tabular-nums">
				{albumCount.toLocaleString()} album{albumCount === 1 ? '' : 's'}
			</span>
		{:else}
			<span class="skeleton skeleton-shimmer h-5 w-20 rounded-full"></span>
		{/if}
	</div>
	<div class="flex items-center gap-2">
		{#if onrefresh}
			<IconButton
				size="sm"
				icon={RefreshCw}
				label="Refresh page data"
				loading={refreshing}
				onclick={onrefresh}
			/>
		{/if}
		{#if settingsSnippet}
			{@render settingsSnippet()}
		{/if}
	</div>
</div>
