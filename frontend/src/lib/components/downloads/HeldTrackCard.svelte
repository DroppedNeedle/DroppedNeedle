<script lang="ts">
	import { TriangleAlert } from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { HeldImport } from '$lib/types';
	import { albumHref } from '$lib/utils/entityRoutes';

	import HeldTrackReview from './HeldTrackReview.svelte';

	let { held }: { held: HeldImport } = $props();
</script>

<article class="rounded-2xl border border-warning/20 bg-base-200/50 p-3 backdrop-blur-sm sm:p-4">
	<div class="flex items-start gap-3 sm:gap-4">
		<div class="size-14 shrink-0 overflow-hidden rounded-lg ring-1 ring-base-content/10 sm:size-16">
			{#if held.release_group_mbid}
				<AlbumImage
					mbid={held.release_group_mbid}
					alt={held.album_title ?? 'Album'}
					size="sm"
					rounded="lg"
					className="h-full w-full"
				/>
			{:else}
				<div class="grid h-full w-full place-items-center bg-base-300">
					<TriangleAlert class="size-6 text-warning" aria-hidden="true" />
				</div>
			{/if}
		</div>

		<div class="min-w-0 flex-1">
			<h3 class="truncate text-sm font-semibold sm:text-base">
				{#if held.track_number}{held.track_number}.
				{/if}{held.track_title ?? held.original_filename ?? 'Unknown track'}
			</h3>
			<p class="truncate text-xs text-base-content/60">
				{held.artist_name ?? 'Unknown artist'}{#if held.album_title}<span
						class="text-base-content/30"
					>
						·
					</span>{#if held.release_group_mbid}<a
							href={albumHref(held.release_group_mbid)}
							class="hover:text-primary transition-colors">{held.album_title}</a
						>{:else}{held.album_title}{/if}{/if}
			</p>
			<p class="mt-1.5 text-[11px] font-bold uppercase tracking-[0.14em] text-warning">
				Couldn't verify
			</p>
			<div class="mt-1.5"><HeldTrackReview {held} /></div>
		</div>
	</div>
</article>
