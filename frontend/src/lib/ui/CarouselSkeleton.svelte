<script lang="ts">
	interface Props {
		count?: number;
		cardWidth?: string;
		showSubtitle?: boolean;
		rounded?: 'lg' | 'full';
	}

	let {
		count = 6,
		cardWidth = 'w-32 sm:w-36 md:w-44',
		showSubtitle = true,
		rounded = 'lg'
	}: Props = $props();
</script>

<div class="flex gap-3 overflow-x-auto pb-2 sm:gap-4 scrollbar-hide" role="status" aria-busy="true">
	<span class="sr-only">Loading…</span>
	{#each Array(count) as _, i (i)}
		<div class="{cardWidth} shrink-0">
			<div
				class="skeleton skeleton-shimmer aspect-square w-full {rounded === 'full'
					? 'rounded-full'
					: 'rounded-lg'}"
			></div>
			<div
				class="skeleton skeleton-shimmer mt-2 h-4 w-3/4 {rounded === 'full' ? 'mx-auto' : ''}"
			></div>
			{#if showSubtitle}
				<div class="skeleton skeleton-shimmer mt-1 h-3 w-1/2"></div>
			{/if}
		</div>
	{/each}
</div>
