<script lang="ts">
	import Card from '$lib/ui/Card.svelte';
</script>

<Card variant="raised" padding="none" class="w-full shrink-0">
	<figure class="aspect-square overflow-hidden relative">
		<div class="skeleton skeleton-shimmer w-full h-full absolute inset-0"></div>
	</figure>
	<div class="flex flex-col gap-2 p-3">
		<div class="skeleton skeleton-shimmer h-5 w-full min-h-[2.5rem]"></div>
		<div class="skeleton skeleton-shimmer h-4 w-3/4"></div>
	</div>
</Card>
