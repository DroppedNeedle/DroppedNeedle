<script lang="ts">
	import { albumHref } from '$lib/utils/entityRoutes';
	import { Play } from 'lucide-svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import type { TopSong, ResolvedTrack } from '$lib/types';
	import AlbumImage from './AlbumImage.svelte';
	import SampleButton from './discover/SampleButton.svelte';

	interface Props {
		song: TopSong;
		position: number;
		source?: string;
		showPreview?: boolean;
		resolvedTrack?: ResolvedTrack | null;
		onPlay?: () => void;
	}

	let {
		song,
		position,
		source = '',
		showPreview = false,
		resolvedTrack = null,
		onPlay
	}: Props = $props();

	let hasAlbum = $derived(!!song.release_group_mbid);
	let isLastfmNoAlbum = $derived(!hasAlbum && source === 'lastfm');
	let canPlay = $derived(!!resolvedTrack?.source);
	let previewEnabled = $derived(showPreview && !canPlay);
</script>

{#if hasAlbum}
	<div class="flex items-center gap-3 p-2 rounded-lg hover:bg-base-200 transition-colors group">
		{#if canPlay}
			<button
				onclick={onPlay}
				class="w-6 shrink-0 flex items-center justify-center cursor-pointer"
				aria-label="Play {song.title}"
			>
				<span class="group-hover:hidden text-sm text-base-content/50">{position}</span>
				<span class="hidden group-hover:block text-primary">
					<Play class="w-4 h-4 mx-auto fill-current" />
				</span>
			</button>
		{:else if previewEnabled}
			<span class="w-6 shrink-0 flex items-center justify-center">
				<span class="group-hover:hidden">{position}</span>
				<span class="hidden group-hover:block">
					<SampleButton
						sampleKey={`track:${song.artist_name}|${song.title}`}
						artist={song.artist_name}
						title={song.title}
						kind="track"
						size="xs"
						albumMbid={song.release_group_mbid}
					/>
				</span>
			</span>
		{:else}
			<a
				href={albumHref(song.release_group_mbid ?? '')}
				class="w-6 shrink-0 flex items-center justify-center"
			>
				<span class="group-hover:hidden text-sm text-base-content/50">{position}</span>
				<span class="hidden group-hover:block">
					<Play class="w-4 h-4 mx-auto fill-current" />
				</span>
			</a>
		{/if}

		<a
			href={albumHref(song.release_group_mbid ?? '')}
			class="flex items-center gap-3 flex-1 min-w-0 cursor-pointer"
		>
			<div class="w-12 h-12 shrink-0">
				<AlbumImage
					mbid={song.release_group_mbid ?? ''}
					alt={song.release_name || ''}
					size="full"
					requestSize={250}
					className="w-12 h-12 rounded"
				/>
			</div>

			<div class="flex-1 min-w-0 grid grid-cols-2 items-center gap-4">
				<p class="font-medium text-sm truncate min-w-0">{song.title}</p>
				<p class="text-xs text-base-content/60 truncate min-w-0 text-right">
					{song.release_name || ''}
				</p>
			</div>
		</a>
	</div>
{:else}
	<div
		class="flex items-center gap-3 p-2 rounded-lg transition-colors group {isLastfmNoAlbum
			? 'opacity-75'
			: ''}"
	>
		{#if canPlay}
			<button
				onclick={onPlay}
				class="w-6 shrink-0 flex items-center justify-center cursor-pointer"
				aria-label="Play {song.title}"
			>
				<span class="group-hover:hidden text-sm text-base-content/50">{position}</span>
				<span class="hidden group-hover:block text-primary">
					<Play class="w-4 h-4 mx-auto fill-current" />
				</span>
			</button>
		{:else if previewEnabled}
			<span class="w-6 shrink-0 flex items-center justify-center">
				<span class="group-hover:hidden">{position}</span>
				<span class="hidden group-hover:block">
					<SampleButton
						sampleKey={`track:${song.artist_name}|${song.title}`}
						artist={song.artist_name}
						title={song.title}
						kind="track"
						size="xs"
						albumMbid={song.release_group_mbid}
					/>
				</span>
			</span>
		{:else}
			<span class="w-6 text-center text-sm text-base-content/50">{position}</span>
		{/if}

		<div class="relative w-12 h-12 shrink-0 overflow-hidden rounded">
			<CoverPlaceholder id={song.release_group_mbid} name={song.title} />
		</div>

		<div class="flex-1 min-w-0 grid grid-cols-2 items-center gap-4">
			<p class="font-medium text-sm truncate min-w-0">{song.title}</p>
			<p class="text-xs text-base-content/40 truncate min-w-0 text-right italic"></p>
		</div>
	</div>
{/if}
