<script lang="ts">
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		title?: string;
		count?: number;
	}

	let { title = 'Albums', count = 12 }: Props = $props();
</script>

<div class="mb-8">
	<div class="flex justify-between items-center mb-4">
		<h2 class="text-2xl font-bold">{title}</h2>
		<div class="skeleton skeleton-shimmer h-6 w-24"></div>
	</div>

	<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4">
		{#each Array(count) as _, i (i)}
			<Card variant="raised" padding="none" class="w-full">
				<figure class="aspect-square overflow-hidden relative">
					<div class="skeleton skeleton-shimmer w-full h-full absolute inset-0"></div>
				</figure>
				<div class="px-4 pt-2 pb-4">
					<div class="skeleton skeleton-shimmer h-5 w-full mb-2"></div>
					<div class="skeleton skeleton-shimmer h-4 w-3/4 mb-1"></div>
					<div class="skeleton skeleton-shimmer h-3 w-1/2"></div>
				</div>
			</Card>
		{/each}
	</div>
</div>
