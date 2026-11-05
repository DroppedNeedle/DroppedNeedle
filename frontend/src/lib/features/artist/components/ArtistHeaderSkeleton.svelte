<!-- The artist hero while it loads: the photo's place, the name, the line and the buttons. -->
<div class="relative mx-auto w-full max-w-7xl px-4 pt-6 pb-8 md:px-6 lg:px-8" aria-busy="true">
	<div
		class="skeleton skeleton-shimmer absolute top-0 right-0 hidden h-full w-[55%] rounded-none opacity-40 md:block"
	></div>
	<div class="skeleton skeleton-shimmer mb-6 h-10 w-10 rounded-full"></div>
	<div class="relative space-y-4 pt-24 md:pt-16">
		<div class="skeleton skeleton-shimmer h-20 w-3/4 max-w-xl md:h-32"></div>
		<div class="skeleton skeleton-shimmer h-4 w-72 max-w-full"></div>
		<div class="flex flex-wrap gap-2 pt-2">
			{#each Array(4) as _, i (i)}
				<div class="skeleton skeleton-shimmer h-10 w-24"></div>
			{/each}
		</div>
	</div>
</div>
