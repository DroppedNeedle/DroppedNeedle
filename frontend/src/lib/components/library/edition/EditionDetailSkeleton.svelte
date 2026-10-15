<div class="flex flex-col gap-5" aria-hidden="true">
	<div class="flex items-start gap-4">
		<div class="skeleton h-20 w-20 shrink-0 rounded-box"></div>
		<div class="flex-1 space-y-2">
			<div class="skeleton h-5 w-2/3"></div>
			<div class="skeleton h-3.5 w-1/2"></div>
			<div class="skeleton h-3.5 w-1/3"></div>
		</div>
	</div>
	<div class="skeleton h-4 w-4/5"></div>
	<div class="flex flex-col gap-1.5">
		{#each Array(6) as _, index (index)}
			<div class="skeleton h-9 w-full"></div>
		{/each}
	</div>
</div>
