<!--
	A hub track table: #, title, artist, album and duration; on a phone the
	artist goes under the title. With `onplay` a row plays the list from
	there; without it the rows only list.
-->
<script lang="ts">
	import './hub.css';
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
		onplay?: (index: number) => void;
		formatDuration?: (seconds: number) => string;
		/** Cap the height and scroll inside (long lists). */
		scroll?: boolean;
	}

	let { tracks, onplay, formatDuration = formatDurationSec, scroll = true }: Props = $props();
</script>

<div class="hub-tt" class:hub-tt--scroll={scroll}>
	<table class="hub-table">
		<thead>
			<tr>
				<th class="hub-num">#</th>
				<th>Title</th>
				<th class="hub-tt__wide">Artist</th>
				<th class="hub-tt__wide">Album</th>
				<th class="hub-time">Duration</th>
				{#if onplay}<th class="hub-tt__go"><span class="sr-only">Play</span></th>{/if}
			</tr>
		</thead>
		<tbody>
			{#each tracks as track, i (track.id + '-' + i)}
				{#if onplay}
					<tr
						class="hub-row"
						onclick={() => onplay(i)}
						onkeydown={(e) =>
							e.target === e.currentTarget &&
							(e.key === 'Enter' || e.key === ' ') &&
							(e.preventDefault(), onplay(i))}
						tabindex="0"
						role="button"
						aria-label="Play {track.title}"
					>
						{@render cells(track, i)}
						<td class="hub-tt__go"><Play class="size-3.5 fill-current" aria-hidden="true" /></td>
					</tr>
				{:else}
					<tr>{@render cells(track, i)}</tr>
				{/if}
			{/each}
		</tbody>
	</table>
</div>

{#snippet cells(track: DiscoveryTrack, i: number)}
	<td class="hub-num">{i + 1}</td>
	<td class="hub-clip">
		<span class="hub-tt__title">{track.title}</span>
		<span class="hub-tt__under">{track.artist_name}</span>
	</td>
	<td class="hub-clip hub-tt__wide">{track.artist_name}</td>
	<td class="hub-clip hub-tt__wide hub-dim">{track.album_name}</td>
	<td class="hub-time">{formatDuration(track.duration_seconds)}</td>
{/snippet}

<style>
	.hub-tt {
		container-type: inline-size;
	}
	.hub-tt--scroll {
		max-height: 32rem;
		overflow-y: auto;
	}
	.hub-tt thead th {
		position: sticky;
		top: 0;
		z-index: 1;
		background: var(--color-base-200);
	}
	.hub-tt table {
		table-layout: fixed;
	}
	@container (min-width: 50rem) {
		th.hub-tt__wide {
			width: 25%;
		}
	}
	.hub-tt__title {
		display: block;
		overflow: hidden;
		font-weight: 500;
		text-overflow: ellipsis;
	}
	.hub-tt__under {
		display: none;
		overflow: hidden;
		font-size: 0.75rem;
		text-overflow: ellipsis;
		color: var(--dn-muted);
	}
	.hub-tt__go {
		width: 2.5rem;
		text-align: center;
		color: var(--dn-muted);
	}
	.hub-tt tr.hub-row:hover .hub-tt__go {
		color: var(--dn-title);
	}

	@container (max-width: 34rem) {
		.hub-tt__wide {
			display: none;
		}
		.hub-tt__under {
			display: block;
		}
	}
	@container (max-width: 24rem) {
		.hub-time {
			display: none;
		}
	}
</style>
