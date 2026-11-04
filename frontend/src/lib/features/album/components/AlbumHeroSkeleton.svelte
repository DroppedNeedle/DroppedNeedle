<!--
	The album hero while it loads: the cover's square, then the eyebrow,
	title, meta line and action row as bars, laid out as the hero is.
	`tracks` adds the track panel's rows under it.
-->
<script lang="ts">
	interface Props {
		tracks?: boolean;
	}

	let { tracks = false }: Props = $props();
</script>

<div class="mx-auto w-full max-w-[80rem] px-4 pt-14 pb-6 md:px-6 lg:px-8" aria-busy="true">
	<span class="sr-only">Loading album</span>
	<div class="grid items-center gap-6 md:grid-cols-[16rem_1fr] md:gap-8 lg:grid-cols-[19rem_1fr]">
		<div
			class="skeleton mx-auto aspect-square w-full max-w-76 rounded-md min-[480px]:mx-0 md:max-w-none lg:w-[72%]"
		></div>
		<div class="space-y-4">
			<div class="skeleton h-3.5 w-20"></div>
			<div class="skeleton h-14 w-4/5 sm:h-16"></div>
			<div class="skeleton h-5 w-1/2"></div>
			<div class="flex gap-3 pt-2">
				<div class="skeleton h-10 w-28"></div>
				<div class="skeleton h-10 w-32"></div>
				<div class="skeleton h-10 w-10"></div>
			</div>
		</div>
	</div>
	{#if tracks}
		<div class="mt-10 space-y-3">
			<div class="skeleton h-7 w-28"></div>
			<div class="space-y-1 rounded-box border border-line p-3">
				{#each Array(8) as _, index (index)}
					<div class="skeleton h-8 w-full"></div>
				{/each}
			</div>
		</div>
	{/if}
</div>
