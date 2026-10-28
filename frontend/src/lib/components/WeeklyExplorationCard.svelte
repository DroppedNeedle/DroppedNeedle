<script lang="ts">
	import type { WeeklyTrack } from '$lib/queries/discover/types';
	import { Music2 } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { albumHrefOrNull, artistHrefOrNull } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import AlbumRequestButton from './AlbumRequestButton.svelte';
	import SampleButton from './discover/SampleButton.svelte';
	import AlbumImage from './AlbumImage.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		track: WeeklyTrack;
	}

	let { track }: Props = $props();

	const albumHref = $derived(albumHrefOrNull(track.release_group_mbid));
	const artistHref = $derived(artistHrefOrNull(track.artist_mbid));
	const albumMbid = $derived(track.release_group_mbid);
	const isRequested = $derived(albumMbid ? libraryStore.isRequested(albumMbid) : false);

	function formatDuration(ms: number | null): string {
		if (!ms) return '';
		const totalSec = Math.floor(ms / 1000);
		const min = Math.floor(totalSec / 60);
		const sec = totalSec % 60;
		return `${min}:${sec.toString().padStart(2, '0')}`;
	}
</script>

<Card variant="raised" padding="none" class="w-44 shrink-0 overflow-hidden">
	<div class="flex flex-1 flex-col">
		{#if albumHref}
			<a
				href={albumHref}
				class="group relative aspect-square w-full overflow-hidden rounded-t-xl bg-base-200"
			>
				{#if track.cover_url}
					<AlbumImage
						customUrl={track.cover_url}
						alt={track.album_name || track.title}
						size="full"
						rounded="none"
						className="h-full w-full transition-transform duration-300 group-hover:scale-105"
					/>
				{:else}
					<div class="relative h-full w-full overflow-hidden">
						<CoverPlaceholder id={albumMbid} name={track.album_name || track.title} />
					</div>
				{/if}

				{#if track.duration_ms}
					<span
						class="absolute bottom-1.5 right-1.5 rounded bg-black/70 px-1.5 py-0.5 text-[10px]
					font-medium text-white/90 backdrop-blur-sm"
					>
						{formatDuration(track.duration_ms)}
					</span>
				{/if}
			</a>
		{:else}
			<div class="relative aspect-square w-full overflow-hidden rounded-t-xl bg-base-200">
				{#if track.cover_url}
					<AlbumImage
						customUrl={track.cover_url}
						alt={track.album_name || track.title}
						size="full"
						rounded="none"
						className="h-full w-full"
					/>
				{:else}
					<div class="relative h-full w-full overflow-hidden">
						<CoverPlaceholder id={albumMbid} name={track.album_name || track.title} />
					</div>
				{/if}

				{#if track.duration_ms}
					<span
						class="absolute bottom-1.5 right-1.5 rounded bg-black/70 px-1.5 py-0.5 text-[10px]
					font-medium text-white/90 backdrop-blur-sm"
					>
						{formatDuration(track.duration_ms)}
					</span>
				{/if}
			</div>
		{/if}

		<div class="flex flex-1 flex-col gap-0.5 p-2.5 pb-1.5">
			<h3 class="text-sm font-semibold leading-tight line-clamp-1 text-base-content">
				{track.title}
			</h3>
			{#if artistHref}
				<a
					href={artistHref}
					class="text-xs text-base-content/60 line-clamp-1 hover:text-primary transition-colors"
				>
					{track.artist_name}
				</a>
			{:else}
				<p class="text-xs text-base-content/60 line-clamp-1">{track.artist_name}</p>
			{/if}
			{#if track.album_name}
				{#if albumHref}
					<a
						href={albumHref}
						class="text-[11px] text-base-content/40 line-clamp-1 flex items-center gap-1
						hover:text-primary transition-colors"
					>
						<Music2 class="h-2.5 w-2.5 shrink-0" />
						{track.album_name}
					</a>
				{:else}
					<p class="text-[11px] text-base-content/40 line-clamp-1 flex items-center gap-1">
						<Music2 class="h-2.5 w-2.5 shrink-0" />
						{track.album_name}
					</p>
				{/if}
			{/if}
		</div>

		<div class="mt-auto flex items-center justify-center gap-3 px-2 pb-1 pt-0.5">
			{#if albumMbid && !isRequested}
				<AlbumRequestButton
					mbid={albumMbid}
					artistName={track.artist_name}
					albumName={track.album_name || track.title}
					artistMbid={track.artist_mbid ?? undefined}
				/>
			{/if}
			<SampleButton
				sampleKey={`track:${track.artist_name}|${track.title}`}
				artist={track.artist_name}
				title={track.title}
				kind="track"
				size="md"
				albumMbid={track.release_group_mbid}
				artistMbid={track.artist_mbid}
				coverUrl={track.cover_url}
			/>
		</div>

		<div class="pb-1.5"></div>
	</div>
</Card>
