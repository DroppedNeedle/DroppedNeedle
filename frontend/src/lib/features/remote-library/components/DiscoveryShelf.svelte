<script lang="ts">
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import { RefreshCw, Sparkles } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

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
	<div class="flex items-center justify-between px-1">
		<div class="flex items-center gap-2">
			<Sparkles class="h-5 w-5 text-secondary" />
			<h2 class="text-lg font-semibold text-base-content sm:text-xl">{title}</h2>
		</div>
		{#if onrefresh}
			<Button variant="ghost" size="sm" icon={RefreshCw} {loading} onclick={onrefresh}>
				Refresh
			</Button>
		{/if}
	</div>

	{#if loading}
		<CarouselSkeleton />
	{:else}
		{#if actions}{@render actions()}{/if}
		{#if empty}
			<Card>
				<p class="text-center text-sm text-base-content/50">{emptyMessage}</p>
			</Card>
		{:else}
			{@render children()}
		{/if}
	{/if}
</section>
