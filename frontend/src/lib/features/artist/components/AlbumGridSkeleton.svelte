<script lang="ts">
	interface Props {
		title?: string;
		count?: number;
	}

	let { title = 'Albums', count = 12 }: Props = $props();
</script>

<div class="mb-8" aria-busy="true">
	<div class="mb-5 flex items-center gap-6">
		<h2 class="font-display text-4xl text-base-content">{title}</h2>
		<div class="skeleton skeleton-shimmer h-8 w-28 rounded-full"></div>
		<div class="skeleton skeleton-shimmer h-8 w-20 rounded-full"></div>
	</div>

	<div class="grid grid-cols-2 gap-5 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6 xl:grid-cols-8">
		{#each Array(count) as _, i (i)}
			<div>
				<div class="skeleton skeleton-shimmer aspect-square w-full"></div>
				<div class="skeleton skeleton-shimmer mt-2 h-4 w-3/4"></div>
				<div class="skeleton skeleton-shimmer mt-1 h-3 w-1/3"></div>
			</div>
		{/each}
	</div>
</div>
