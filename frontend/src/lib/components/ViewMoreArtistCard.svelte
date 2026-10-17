<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { withBasePath } from '$lib/utils/basePath';
	import { ChevronRight } from 'lucide-svelte';
	import AlbumImage from './AlbumImage.svelte';

	function handleClick() {
		const query = page.url.searchParams.get('q') || '';
		if (query) {
			goto(withBasePath(`/search/artists?q=${encodeURIComponent(query)}`));
		}
	}
</script>

<button
	class="relative w-full aspect-square cursor-pointer transition-transform duration-200 ease-in-out hover:scale-105 rounded-full overflow-hidden"
	onclick={handleClick}
>
	<div class="absolute inset-0 overflow-hidden bg-base-200">
		<AlbumImage
			customUrl={withBasePath('/img/artist_bg-250.webp')}
			alt=""
			size="full"
			rounded="none"
			showPlaceholder={false}
			lazy={false}
			testId="view-more-artist-background"
			className="h-full w-full blur-[8px]"
		/>
	</div>
	<div class="absolute inset-0 z-10 flex flex-col items-center justify-center p-2">
		<div
			class="px-2 py-1 rounded-lg bg-base-300/40 light:bg-base-100/80 text-accent backdrop-blur-sm shadow-lg max-w-full"
		>
			<div class="flex flex-col items-center gap-0.5">
				<span class="text-sm font-bold block text-center drop-shadow-lg truncate">ARTISTS</span>
				<div class="flex items-center gap-1">
					<span class="text-xs font-semibold text-center drop-shadow-lg">View More</span>
					<ChevronRight class="h-3 w-3 drop-shadow-lg" strokeWidth={2.5} />
				</div>
			</div>
		</div>
	</div>
</button>
