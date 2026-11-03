<script lang="ts">
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		variant?: 'compact' | 'detailed';
	}

	let { variant = 'compact' }: Props = $props();
</script>

{#if variant === 'detailed'}
	<Card variant="raised" padding="none" class="w-full shrink-0">
		<figure class="flex justify-center pt-4">
			<div class="skeleton skeleton-shimmer w-36 h-36 sm:w-44 sm:h-44 rounded-full"></div>
		</figure>
		<div class="flex flex-col items-center text-center p-3 gap-1">
			<div class="skeleton skeleton-shimmer h-4 w-3/4"></div>
			<div class="skeleton skeleton-shimmer h-3 w-1/2"></div>
			<div class="skeleton skeleton-shimmer h-5 w-2/3 mt-1"></div>
		</div>
	</Card>
{:else}
	<Card variant="raised" padding="none" class="w-full shrink-0">
		<figure class="flex justify-center pt-4">
			<div class="skeleton skeleton-shimmer w-24 h-24 sm:w-32 sm:h-32 rounded-full"></div>
		</figure>
		<div class="flex flex-col items-center text-center p-2">
			<div class="skeleton skeleton-shimmer h-4 w-3/4"></div>
		</div>
	</Card>
{/if}
