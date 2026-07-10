<script lang="ts">
	import { Play, Shuffle, ListPlus, CirclePlus, Loader2 } from 'lucide-svelte';
	import { PersistedState } from 'runed';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { openGlobalPlaylistModal } from '$lib/components/AddToPlaylistModal.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/components/LocalFilesIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { playerStore } from '$lib/stores/player.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { getSourceColor, getSourceLabel } from '$lib/utils/sources';
	import { createArtistTrackLoader } from '$lib/utils/artistTrackLoader.svelte';
	import type { ReleaseGroup } from '$lib/types';
	import type { SourceType } from '$lib/player/types';

	type PlaybackSourceType = Exclude<SourceType, 'youtube'>;

	interface Props {
		artistName: string;
		artistId: string;
		releases: ReleaseGroup[];
	}

	let { artistName, artistId, releases }: Props = $props();

	const artistPlaybackSources = new PersistedState<Record<string, PlaybackSourceType>>(
		'artist-playback-sources',
		{}
	);
	const lastPlaybackSource = new PersistedState<PlaybackSourceType | null>(
		'last-playback-source',
		null
	);

	const PLAYBACK_SOURCES: { key: PlaybackSourceType; integrationKey: string }[] = [
		{ key: 'local', integrationKey: 'localfiles' },
		{ key: 'navidrome', integrationKey: 'navidrome' },
		{ key: 'jellyfin', integrationKey: 'jellyfin' },
		{ key: 'plex', integrationKey: 'plex' }
	];

	let availableSources = $derived.by(() => {
		const status = $integrationStore;
		return PLAYBACK_SOURCES.filter(
			(s) => status[s.integrationKey as keyof typeof status] === true
		).map((s) => s.key);
	});

	let inLibraryReleases = $derived(releases.filter((r) => r.in_library));
	let showBar = $derived(inLibraryReleases.length > 0 && availableSources.length > 0);

	let selectedSource = $derived.by<PlaybackSourceType>(() => {
		const perArtist = artistPlaybackSources.current[artistId];
		if (perArtist && availableSources.includes(perArtist)) return perArtist;

		const lastUsed = lastPlaybackSource.current;
		if (lastUsed && availableSources.includes(lastUsed)) return lastUsed;

		return availableSources[0] ?? 'local';
	});

	function setSource(source: PlaybackSourceType) {
		artistPlaybackSources.current = {
			...artistPlaybackSources.current,
			[artistId]: source
		};
		lastPlaybackSource.current = source;
	}

	const loader = createArtistTrackLoader(
		() => ({
			artistName,
			artistId,
			releases: inLibraryReleases,
			source: selectedSource
		}),
		(items, startIndex, shuffle) => playerStore.playQueue(items, startIndex, shuffle),
		(items) => playerStore.appendQueueSilent(items),
		() => playerStore.regenerateShuffleOrder(),
		(message, type) => toastStore.show({ message, type: type === 'error' ? 'error' : 'info' })
	);

	async function handleAddToQueue() {
		const tracks = await loader.fetchTracks();
		if (tracks.length > 0) {
			playerStore.addMultipleToQueue(tracks);
			toastStore.show({
				message: `Added ${tracks.length} tracks to queue from ${artistName}`,
				type: 'success'
			});
		}
	}

	async function handleAddToPlaylist() {
		const tracks = await loader.fetchTracks();
		if (tracks.length > 0) {
			openGlobalPlaylistModal(tracks);
		}
	}

	let menuItems = $derived.by<MenuItem[]>(() => [
		{ label: 'Add All to Queue', icon: ListPlus, onclick: handleAddToQueue },
		{ label: 'Add All to Playlist', icon: CirclePlus, onclick: handleAddToPlaylist }
	]);

	$effect(() => {
		return () => loader.abort();
	});
</script>

{#snippet sourceIcon(source: PlaybackSourceType, cls: string)}
	{#if source === 'local'}
		<LocalFilesIcon class={cls} />
	{:else if source === 'navidrome'}
		<NavidromeIcon class={cls} />
	{:else if source === 'jellyfin'}
		<JellyfinIcon class={cls} />
	{:else if source === 'plex'}
		<PlexIcon class={cls} />
	{/if}
{/snippet}

{#if showBar}
	<div
		class="rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:p-4"
		role="toolbar"
		aria-label="Artist playback controls"
	>
		<div class="flex flex-col sm:flex-row sm:items-center gap-3">
			<div class="flex items-center gap-2.5 min-w-0">
				<span
					class="select-none whitespace-nowrap font-mono text-[0.62rem] font-bold uppercase tracking-[0.18em] text-base-content/45"
					>Play from</span
				>
				{#if availableSources.length > 1}
					<div class="join">
						{#each availableSources as source (source)}
							{@const isActive = source === selectedSource}
							<button
								type="button"
								class="join-item btn btn-xs h-8 gap-1.5 border-base-content/10 transition-all duration-150"
								class:btn-active={isActive}
								style={isActive
									? `background-color: color-mix(in srgb, ${getSourceColor(source)} 15%, transparent); color: ${getSourceColor(source)}; border-color: color-mix(in srgb, ${getSourceColor(source)} 25%, transparent);`
									: ''}
								aria-pressed={isActive}
								onclick={() => setSource(source)}
							>
								{@render sourceIcon(source, 'h-3.5 w-3.5')}
								<span class="hidden md:inline text-xs font-medium">{getSourceLabel(source)}</span>
							</button>
						{/each}
					</div>
				{:else if availableSources.length === 1}
					<span
						class="badge badge-sm gap-1.5 border-base-content/10"
						style="color: {getSourceColor(availableSources[0])};"
					>
						{@render sourceIcon(availableSources[0], 'h-3 w-3')}
						<span class="text-xs font-medium">{getSourceLabel(availableSources[0])}</span>
					</span>
				{/if}
			</div>

			<div class="flex items-center gap-2 sm:ml-auto">
				<button
					class="btn btn-sm btn-primary gap-1.5 rounded-full shadow-sm"
					disabled={loader.loading}
					aria-label="Play all tracks from {artistName}"
					onclick={() => loader.playAll()}
				>
					{#if loader.loading}
						<Loader2 class="h-4 w-4 animate-spin" />
					{:else}
						<Play class="h-4 w-4 fill-current" />
					{/if}
					Play All
				</button>

				<button
					class="btn btn-sm btn-ghost gap-1.5 rounded-full border border-base-content/10"
					disabled={loader.loading}
					aria-label="Shuffle all tracks from {artistName}"
					onclick={() => loader.shuffleAll()}
				>
					<Shuffle class="h-4 w-4" />
					Shuffle
				</button>

				<ContextMenu items={menuItems} position="end" size="sm" />
			</div>
		</div>

		{#if loader.progressText}
			<p class="text-xs text-base-content/40 mt-2 animate-pulse" aria-live="polite">
				{loader.progressText}
			</p>
		{/if}
	</div>
{/if}
