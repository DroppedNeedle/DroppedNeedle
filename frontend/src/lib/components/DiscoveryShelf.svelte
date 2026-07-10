<script lang="ts">
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import { RefreshCw, Sparkles } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		loading?: boolean;
		empty?: boolean;
		emptyMessage?: string;
		onrefresh?: () => void;
		actions?: Snippet;
		children: Snippet;
	}

	let {
		title,
		loading = false,
		empty = false,
		emptyMessage = 'Nothing here yet.',
		onrefresh,
		actions,
		children
	}: Props = $props();
</script>

<section class="space-y-3">
	<div class="flex items-center justify-between gap-3 px-1">
		<h2
			class="flex min-w-0 items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
		>
			<Sparkles class="h-4 w-4 shrink-0 text-accent" />
			<span class="truncate">{title}</span>
		</h2>
		{#if onrefresh}
			<button
				class="btn btn-ghost btn-xs shrink-0 gap-1.5 rounded-full border border-base-content/10 font-mono text-[0.62rem] uppercase tracking-wider text-base-content/60 transition-colors hover:border-primary/30 hover:text-base-content"
				onclick={onrefresh}
				disabled={loading}
			>
				<span class:animate-spin={loading}>
					<RefreshCw class="h-3.5 w-3.5" />
				</span>
				Refresh
			</button>
		{/if}
	</div>

	{#if loading}
		<CarouselSkeleton />
	{:else}
		{#if actions}{@render actions()}{/if}
		{#if empty}
			<div
				class="rounded-2xl border border-dashed border-base-content/10 bg-base-200/30 p-8 text-center"
			>
				<p class="text-sm text-base-content/50">{emptyMessage}</p>
			</div>
		{:else}
			{@render children()}
		{/if}
	{/if}
</section>
