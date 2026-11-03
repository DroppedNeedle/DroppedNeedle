<script lang="ts">
	import Card from '$lib/ui/Card.svelte';
</script>

<section
	class="overflow-hidden rounded-2xl border border-base-content/10 bg-base-200/30 p-4 sm:p-6"
>
	<div class="flex items-center gap-3">
		<div class="skeleton skeleton-shimmer h-11 w-11 rounded-xl"></div>
		<div class="space-y-2">
			<div class="skeleton skeleton-shimmer h-5 w-48"></div>
			<div class="skeleton skeleton-shimmer h-3 w-72 max-w-full"></div>
		</div>
	</div>

	<div class="mt-5 space-y-3">
		{#each Array(2) as _, index (index)}
			<Card variant="bordered" padding="sm">
				<div class="grid gap-4 sm:grid-cols-[7rem_minmax(0,1fr)]">
					<div class="skeleton skeleton-shimmer aspect-square w-full rounded-lg"></div>
					<div class="space-y-3">
						<div class="skeleton skeleton-shimmer h-6 w-2/5"></div>
						<div class="skeleton skeleton-shimmer h-4 w-1/4"></div>
						<div class="skeleton skeleton-shimmer h-12 w-full"></div>
					</div>
				</div>
			</Card>
		{/each}
	</div>
</section>
