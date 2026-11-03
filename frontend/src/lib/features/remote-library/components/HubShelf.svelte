<script lang="ts">
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import { ChevronRight } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		title: string;
		seeAllHref?: string;
		loading?: boolean;
		children: Snippet;
	}

	let { title, seeAllHref, loading = false, children }: Props = $props();
</script>

<section class="space-y-3">
	<div class="flex items-center justify-between px-1">
		{#if seeAllHref}
			<a
				href={withBasePath(seeAllHref)}
				class="group/title flex items-center gap-1 transition-colors hover:text-primary"
			>
				<h2
					class="text-lg font-semibold text-base-content group-hover/title:text-primary sm:text-xl"
				>
					{title}
				</h2>
			</a>
		{:else}
			<h2 class="text-lg font-semibold text-base-content sm:text-xl">{title}</h2>
		{/if}
		{#if seeAllHref}
			<Button
				variant="ghost"
				size="sm"
				icon={ChevronRight}
				iconPosition="end"
				href={withBasePath(seeAllHref)}
			>
				View all
			</Button>
		{/if}
	</div>

	<Card variant="raised" padding="md">
		{#if loading}
			<CarouselSkeleton />
		{:else}
			{@render children()}
		{/if}
	</Card>
</section>
