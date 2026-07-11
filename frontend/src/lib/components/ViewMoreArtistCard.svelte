<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { colors } from '$lib/colors';
	import { ChevronRight } from 'lucide-svelte';

	let imgError = $state(false);

	function handleError() {
		imgError = true;
	}

	function handleClick() {
		const query = page.url.searchParams.get('q') || '';
		if (query) {
			goto(resolve(`/search/artists?q=${encodeURIComponent(query)}`));
		}
	}
</script>

<button
	class="relative w-full aspect-square cursor-pointer transition-transform duration-200 ease-in-out hover:scale-105 rounded-full overflow-hidden"
	onclick={handleClick}
>
	{#if imgError}
		<div class="absolute inset-0 bg-base-200"></div>
	{:else}
		<div class="absolute inset-0 overflow-hidden">
			<img
				data-testid="view-more-artist-background"
				src="/img/artist_bg-250.webp"
				srcset="/img/artist_bg-250.webp 250w, /img/artist_bg-500.webp 500w"
				sizes="200px"
				width="500"
				height="500"
				alt=""
				class="w-full h-full object-cover"
				style="filter: blur(8px);"
				decoding="async"
				onerror={handleError}
			/>
		</div>
	{/if}
	<div class="absolute inset-0 z-10 flex flex-col items-center justify-center p-2">
		<div
			class="px-2 py-1 rounded-lg backdrop-blur-sm shadow-lg max-w-full"
			style="background-color: {colors.secondary}66;"
		>
			<div class="flex flex-col items-center gap-0.5">
				<span
					class="text-sm font-bold block text-center drop-shadow-lg truncate"
					style="color: {colors.accent};">ARTISTS</span
				>
				<div class="flex items-center gap-1">
					<span
						class="text-xs font-semibold text-center drop-shadow-lg"
						style="color: {colors.accent};">View More</span
					>
					<ChevronRight class="h-3 w-3 drop-shadow-lg" color={colors.accent} strokeWidth={2.5} />
				</div>
			</div>
		</div>
	</div>
</button>
