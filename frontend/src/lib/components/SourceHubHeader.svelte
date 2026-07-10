<script lang="ts">
	import { RefreshCw } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

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

<header class="flex flex-wrap items-end justify-between gap-4 px-1 pb-2 pt-2">
	<div class="flex min-w-0 items-center gap-4">
		<span
			class="flex h-14 w-14 shrink-0 items-center justify-center rounded-2xl border border-base-content/8 bg-base-200/50 text-primary"
		>
			{@render icon()}
		</span>
		<div class="min-w-0">
			<p
				class="flex items-center gap-2.5 font-mono text-[0.62rem] font-bold uppercase tracking-[0.22em] text-base-content/50"
			>
				Source Library
				{#if albumCount !== null}
					<span class="h-1 w-1 shrink-0 rounded-full bg-base-content/30" aria-hidden="true"></span>
					<span class="tabular-nums">
						{albumCount.toLocaleString()} album{albumCount === 1 ? '' : 's'}
					</span>
				{:else}
					<span class="skeleton skeleton-shimmer h-3 w-16 rounded-full"></span>
				{/if}
			</p>
			<h1
				class="mt-1 truncate font-display text-3xl font-bold tracking-tight text-base-content sm:text-4xl"
			>
				{title}
			</h1>
		</div>
	</div>
	<div class="flex items-center gap-2">
		{#if onrefresh}
			<button
				class="btn btn-ghost btn-sm gap-2 rounded-full border border-base-content/10 font-medium text-base-content/70 transition-colors hover:border-primary/30 hover:text-base-content"
				onclick={onrefresh}
				disabled={refreshing}
				aria-label="Refresh page data"
			>
				<RefreshCw class="h-3.5 w-3.5 {refreshing ? 'animate-spin' : ''}" />
				Refresh
			</button>
		{/if}
		{#if settingsSnippet}
			{@render settingsSnippet()}
		{/if}
	</div>
</header>
