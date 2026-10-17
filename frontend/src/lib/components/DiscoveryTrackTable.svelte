<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { formatDurationSec } from '$lib/utils/formatting';
	import { Play } from 'lucide-svelte';

	export type DiscoveryTrack = {
		id: string;
		title: string;
		artist_name: string;
		album_name: string;
		album_id?: string;
		image_url?: string | null;
		duration_seconds: number;
	};

	interface Props {
		tracks: DiscoveryTrack[];
		onplay: (index: number) => void;
		formatDuration?: (seconds: number) => string;
	}

	let { tracks, onplay, formatDuration = formatDurationSec }: Props = $props();
</script>

<div class="max-h-[32rem] overflow-y-auto rounded-xl">
	<table class="table table-sm w-full">
		<thead class="sticky top-0 z-10 bg-base-200/80 backdrop-blur-sm">
			<tr class="text-xs uppercase tracking-wider text-base-content/40">
				<th class="w-10">#</th>
				<th>Title</th>
				<th class="hidden sm:table-cell">Album</th>
				<th class="w-16 text-right">Time</th>
			</tr>
		</thead>
		<tbody>
			{#each tracks as track, i (track.id)}
				<tr
					class="group cursor-pointer border-l-2 border-l-transparent transition-all duration-150 hover:border-l-primary hover:bg-base-content/[0.03] focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-primary"
					onclick={() => onplay(i)}
					onkeydown={(e) =>
						e.target === e.currentTarget &&
						(e.key === 'Enter' || e.key === ' ') &&
						(e.preventDefault(), onplay(i))}
					tabindex="0"
					role="button"
					aria-label="Play {track.title}"
				>
					<td class="w-10">
						<span class="text-base-content/40 group-hover:hidden group-focus-visible:hidden"
							>{i + 1}</span
						>
						<span class="hidden text-primary group-hover:inline group-focus-visible:inline">
							<Play class="h-3.5 w-3.5 fill-current" />
						</span>
					</td>
					<td>
						<div class="flex items-center gap-3">
							<div class="hidden h-8 w-8 shrink-0 overflow-hidden rounded sm:block">
								<AlbumImage
									mbid={track.album_id ?? track.id}
									customUrl={track.image_url}
									alt={track.album_name}
									size="xs"
									rounded="none"
									className="h-full w-full object-cover"
								/>
							</div>
							<div class="min-w-0">
								<p class="truncate text-sm font-medium text-base-content">
									{track.title}
								</p>
								<p class="truncate text-xs text-base-content/50">
									{track.artist_name}
								</p>
							</div>
						</div>
					</td>
					<td class="hidden text-sm text-base-content/50 sm:table-cell">
						<span class="line-clamp-1">{track.album_name}</span>
					</td>
					<td class="w-16 text-right font-mono text-xs text-base-content/40">
						{formatDuration(track.duration_seconds)}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>
