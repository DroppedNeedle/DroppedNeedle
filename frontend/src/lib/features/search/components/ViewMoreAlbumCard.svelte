<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { withBasePath } from '$lib/utils/basePath';
	import { ChevronRight } from 'lucide-svelte';

	function handleClick() {
		const query = page.url.searchParams.get('q') || '';
		if (query) {
			goto(withBasePath(`/search/albums?q=${encodeURIComponent(query)}`));
		}
	}
</script>

<!-- The last tile of a search row: the same box as the tiles beside it, on
     a plain surface, opening the full list. -->
<button
	type="button"
	class="flex aspect-square w-full cursor-pointer flex-col items-center justify-center gap-1 rounded-lg border border-edge bg-base-200 p-2 text-center transition-colors duration-150 hover:bg-neutral"
	onclick={handleClick}
>
	<span class="font-display text-xl leading-none text-title">ALBUMS</span>
	<span class="flex items-center gap-1 font-mono text-xs font-semibold uppercase text-base-content">
		View More
		<ChevronRight class="h-3 w-3" strokeWidth={2.5} />
	</span>
</button>
