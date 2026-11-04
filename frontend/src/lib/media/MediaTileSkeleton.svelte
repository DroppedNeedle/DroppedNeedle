<!--
	A MediaTile while its data loads: the art's box and the two text lines.
	Under reduced motion the sheen stops and a still tint stays.
-->
<script lang="ts">
	interface Props {
		kind?: 'album' | 'artist';
		/** Layout only: width, flex. */
		class?: string;
	}

	let { kind = 'album', class: className = '' }: Props = $props();
</script>

<div class="min-w-0 {className}" aria-hidden="true">
	{#if kind === 'album'}
		<div class="skeleton skeleton-shimmer aspect-square w-full rounded-lg"></div>
		<div class="space-y-2 px-1 pt-2.5">
			<div class="skeleton skeleton-shimmer h-4 w-5/6"></div>
			<div class="skeleton skeleton-shimmer h-3 w-1/2"></div>
		</div>
	{:else}
		<div class="skeleton skeleton-shimmer mx-auto mt-2 aspect-square w-[82%] rounded-full"></div>
		<div class="flex flex-col items-center gap-2 px-1 pt-2.5">
			<div class="skeleton skeleton-shimmer h-4 w-2/3"></div>
			<div class="skeleton skeleton-shimmer h-3 w-1/3"></div>
		</div>
	{/if}
</div>
