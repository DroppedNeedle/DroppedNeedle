<div class="flex flex-col gap-4" aria-hidden="true">
	{#each [2, 4] as rows, group (group)}
		<div>
			<div class="skeleton mb-2 h-3 w-28"></div>
			<div class="flex flex-col gap-1">
				{#each Array(rows) as _, index (index)}
					<div
						class="flex min-h-16 items-center gap-3 rounded-box border border-base-content/10 px-3 py-2"
					>
						<div class="skeleton h-10 w-10 shrink-0 rounded-md"></div>
						<div class="flex-1 space-y-1.5">
							<div class="skeleton h-3.5 w-2/3"></div>
							<div class="skeleton h-3 w-1/2"></div>
						</div>
					</div>
				{/each}
			</div>
		</div>
	{/each}
</div>
