<script lang="ts">
</script>

<div class="relative w-full min-h-105 md:min-h-120 mb-8">
	<div class="absolute inset-0 bg-linear-to-b from-base-300/80 via-base-300/40 to-base-100"></div>

	<div
		class="relative z-10 flex flex-col items-center justify-end h-full min-h-10 md:min-h-120 pb-8 pt-16 px-4"
	>
		<div
			class="skeleton skeleton-shimmer w-40 h-40 md:w-56 md:h-56 rounded-full mb-6 shadow-2xl"
		></div>

		<div class="skeleton skeleton-shimmer h-12 md:h-16 w-64 md:w-96 mb-4"></div>

		<div class="flex flex-wrap justify-center gap-3 mb-4">
			<div class="skeleton skeleton-shimmer h-6 w-20 rounded-full"></div>
			<div class="skeleton skeleton-shimmer h-6 w-28 rounded-full"></div>
			<div class="skeleton skeleton-shimmer h-6 w-24 rounded-full"></div>
		</div>

		<div class="flex flex-wrap justify-center gap-2 mb-6 max-w-2xl">
			{#each Array(5) as _, i (i)}
				<div class="skeleton skeleton-shimmer h-7 w-16 rounded-full"></div>
			{/each}
		</div>

		<div class="w-full max-w-2xl bg-base-200/50 rounded-lg p-4">
			<div class="space-y-2">
				<div class="skeleton skeleton-shimmer h-4 w-full"></div>
				<div class="skeleton skeleton-shimmer h-4 w-full"></div>
				<div class="skeleton skeleton-shimmer h-4 w-4/5"></div>
			</div>
		</div>
	</div>
</div>
