<script lang="ts">
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import { ChevronRight } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		seeAllHref?: string;
		loading?: boolean;
		children: Snippet;
	}

	let { title, seeAllHref, loading = false, children }: Props = $props();
</script>

<section class="space-y-3">
	<div class="flex items-center justify-between gap-3 px-1">
		{#if seeAllHref}
			<a href={seeAllHref} class="group/title min-w-0">
				<h2
					class="truncate font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50 transition-colors group-hover/title:text-primary"
				>
					{title}
				</h2>
			</a>
		{:else}
			<h2
				class="truncate font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
			>
				{title}
			</h2>
		{/if}
		{#if seeAllHref}
			<a
				href={seeAllHref}
				class="btn btn-ghost btn-xs shrink-0 gap-1 rounded-full border border-base-content/10 font-mono text-[0.62rem] uppercase tracking-wider text-base-content/60 transition-colors hover:border-primary/30 hover:text-base-content"
			>
				View all
				<ChevronRight class="h-3.5 w-3.5" />
			</a>
		{/if}
	</div>

	<div class="rounded-2xl border border-base-content/8 bg-base-200/50 p-4 sm:p-5">
		{#if loading}
			<CarouselSkeleton />
		{:else}
			{@render children()}
		{/if}
	</div>
</section>
