<script lang="ts">
	import { resolve } from '$app/paths';
	import type { TopSong, ResolvedTrack } from '$lib/types';
	import type { QueueItem, SourceType } from '$lib/player/types';
	import { API } from '$lib/constants';
	import { api } from '$lib/api/client';
	import { playerStore } from '$lib/stores/player.svelte';
	import TrackRow from './TrackRow.svelte';
	import { SvelteMap } from 'svelte/reactivity';

	interface Props {
		songs: TopSong[];
		loading?: boolean;
		configured?: boolean;
		source?: string;
	}

	let { songs, loading = false, configured = true, source = '' }: Props = $props();

	let resolveMap = new SvelteMap<string, ResolvedTrack>();
	let lastResolveKey = $state('');

	function resolveKey(rgMbid: string, disc: number, track: number): string {
		return `${rgMbid}|${disc}|${track}`;
	}

	function resolvableFingerprint(s: TopSong[]): string {
		return s
			.filter((t) => t.release_group_mbid && t.track_number != null)
			.map((t) => `${t.release_group_mbid}|${t.disc_number ?? 1}|${t.track_number}`)
			.join(';');
	}

	$effect(() => {
		const resolvable = songs.filter((s) => s.release_group_mbid && s.track_number != null);
		if (resolvable.length === 0) return;
		const key = resolvableFingerprint(songs);
		if (key === lastResolveKey) return;
		lastResolveKey = key;

		(async () => {
			try {
				const data = await api.global.post<{ items: ResolvedTrack[] }>(
					API.library.resolveTracks(),
					{
						items: resolvable.map((s) => ({
							release_group_mbid: s.release_group_mbid,
							disc_number: s.disc_number ?? 1,
							track_number: s.track_number
						}))
					}
				);
				if (lastResolveKey === key) {
					for (const item of data.items) {
						if (
							item.source &&
							item.track_source_id &&
							item.release_group_mbid &&
							item.track_number != null
						) {
							resolveMap.set(
								resolveKey(item.release_group_mbid, item.disc_number ?? 1, item.track_number),
								item
							);
						}
					}
				}
			} catch {
				// resolve is best-effort
			}
		})();
	});

	function getResolvedTrack(song: TopSong): ResolvedTrack | null {
		if (!song.release_group_mbid || song.track_number == null) return null;
		return (
			resolveMap.get(
				resolveKey(song.release_group_mbid, song.disc_number ?? 1, song.track_number)
			) ?? null
		);
	}

	function buildQueueItems(startSong: TopSong): { items: QueueItem[]; startIndex: number } {
		const items: QueueItem[] = [];
		let startIndex = 0;

		for (const song of songs) {
			const resolved = getResolvedTrack(song);
			if (!resolved?.source || !resolved?.track_source_id) continue;

			if (song === startSong) startIndex = items.length;

			items.push({
				trackSourceId: resolved.track_source_id,
				trackName: song.title,
				artistName: song.artist_name,
				trackNumber: song.track_number ?? 0,
				albumId: song.release_group_mbid ?? '',
				albumName: song.release_name ?? '',
				coverUrl: null,
				sourceType: resolved.source as SourceType,
				streamUrl: resolved.stream_url ?? undefined,
				format: resolved.format ?? undefined,
				duration: resolved.duration ?? undefined
			});
		}

		return { items, startIndex };
	}

	function handlePlay(song: TopSong) {
		const { items, startIndex } = buildQueueItems(song);
		if (items.length > 0) {
			playerStore.playQueue(items, startIndex);
		}
	}
</script>

<div class="flex flex-col min-w-0">
	<h3 class="text-lg font-semibold mb-3">Popular Songs</h3>

	{#if loading}
		<div class="space-y-2">
			{#each Array(10) as _, i (`skeleton-${i}`)}
				<div class="flex items-center gap-3 p-2">
					<div class="skeleton w-6 h-4"></div>
					<div class="skeleton w-12 h-12 rounded"></div>
					<div class="flex-1 flex items-center gap-4">
						<div class="skeleton h-4 w-1/2"></div>
						<div class="skeleton h-3 w-1/3 ml-auto"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if !configured}
		<div class="bg-base-200 rounded-lg p-4 text-center flex-1 flex items-center justify-center">
			<div>
				<p class="text-base-content/70 text-sm">Connect a music service to see popular songs</p>
				<a href={resolve('/profile#scrobbling')} class="btn btn-primary btn-xs mt-2">Configure</a>
			</div>
		</div>
	{:else if songs.length === 0}
		<div class="bg-base-200 rounded-lg p-4 text-center flex-1 flex items-center justify-center">
			<p class="text-base-content/70 text-sm">No song data available</p>
		</div>
	{:else}
		<div class="space-y-1">
			{#each songs as song, i (song.recording_mbid || `song-${i}`)}
				<TrackRow
					{song}
					position={i + 1}
					{source}
					showPreview={true}
					resolvedTrack={getResolvedTrack(song)}
					onPlay={() => handlePlay(song)}
				/>
			{/each}
		</div>
	{/if}
</div>
