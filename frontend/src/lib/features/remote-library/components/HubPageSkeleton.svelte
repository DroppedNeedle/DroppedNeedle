<script lang="ts">
	import './hub.css';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
</script>

<div class="space-y-10" role="status" aria-busy="true">
	<span class="sr-only">Loading hub...</span>

	<div class="hub-pair">
		{#each [0, 1] as i (i)}
			<div class="space-y-3">
				<div class="skeleton skeleton-shimmer h-8 w-56 rounded"></div>
				<div class="hub-panel flex gap-5">
					<div class="skeleton skeleton-shimmer size-40 shrink-0 rounded"></div>
					<div class="flex-1 space-y-3 pt-2">
						<div class="skeleton skeleton-shimmer h-8 w-3/4 rounded"></div>
						<div class="skeleton skeleton-shimmer h-4 w-1/2 rounded"></div>
						<div class="skeleton skeleton-shimmer h-10 w-48 rounded"></div>
					</div>
				</div>
			</div>
		{/each}
	</div>

	{#each ['Favorites', 'Recently Added'] as title (title)}
		<section class="space-y-3">
			<div class="skeleton skeleton-shimmer h-8 w-48 rounded"></div>
			<CarouselSkeleton />
		</section>
	{/each}
</div>
