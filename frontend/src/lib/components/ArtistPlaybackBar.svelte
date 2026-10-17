<script lang="ts">
	import { Play, Shuffle, ListPlus, CirclePlus } from 'lucide-svelte';
	import { PersistedState } from 'runed';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { openGlobalPlaylistModal } from '$lib/stores/playlistModal.svelte';
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
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

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
	<Card variant="bordered" padding="sm" role="toolbar" aria-label="Artist playback controls">
		<div class="flex flex-col sm:flex-row sm:items-center gap-3">
			<div class="flex items-center gap-2.5 min-w-0">
				<span class="text-xs font-medium text-base-content/50 whitespace-nowrap select-none"
					>Play from:</span
				>
				{#if availableSources.length > 1}
					<div class="join">
						{#each availableSources as source (source)}
							{@const isActive = source === selectedSource}
							<Button
								size="sm"
								class="join-item"
								active={isActive}
								style={isActive
									? `background-color: color-mix(in srgb, ${getSourceColor(source)} 15%, transparent); color: ${getSourceColor(source)}; border-color: color-mix(in srgb, ${getSourceColor(source)} 25%, transparent);`
									: undefined}
								aria-pressed={isActive}
								onclick={() => setSource(source)}
							>
								{@render sourceIcon(source, 'h-3.5 w-3.5')}
								<span class="hidden md:inline text-xs font-medium">{getSourceLabel(source)}</span>
							</Button>
						{/each}
					</div>
				{:else if availableSources.length === 1}
					<span
						class="badge badge-sm gap-1.5 border-base-content/10"
						style:color={getSourceColor(availableSources[0])}
					>
						{@render sourceIcon(availableSources[0], 'h-3 w-3')}
						<span class="text-xs font-medium">{getSourceLabel(availableSources[0])}</span>
					</span>
				{/if}
			</div>

			<div class="flex items-center gap-2 sm:ml-auto">
				<Button
					variant="accent"
					size="sm"
					icon={Play}
					class="[&_svg]:fill-current"
					loading={loader.loading}
					aria-label="Play all tracks from {artistName}"
					onclick={() => loader.playAll()}
				>
					Play All
				</Button>

				<Button
					variant="ghost"
					size="sm"
					icon={Shuffle}
					disabled={loader.loading}
					aria-label="Shuffle all tracks from {artistName}"
					onclick={() => loader.shuffleAll()}
				>
					Shuffle
				</Button>

				<ContextMenu items={menuItems} position="end" size="sm" />
			</div>
		</div>

		{#if loader.progressText}
			<p class="text-xs text-base-content/40 mt-2 animate-pulse" aria-live="polite">
				{loader.progressText}
			</p>
		{/if}
	</Card>
{/if}
