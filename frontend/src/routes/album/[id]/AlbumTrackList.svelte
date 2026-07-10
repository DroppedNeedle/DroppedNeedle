<script lang="ts">
	import type {
		AlbumBasicInfo,
		YouTubeTrackLink,
		YouTubeQuotaStatus,
		JellyfinAlbumMatch,
		JellyfinTrackInfo,
		LocalAlbumMatch,
		LocalTrackInfo,
		NavidromeAlbumMatch,
		NavidromeTrackInfo,
		PlexAlbumMatch,
		PlexTrackInfo
	} from '$lib/types';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import type { RenderedTrackSection } from './albumTrackResolvers';
	import { resolveSourceTrack } from './albumTrackResolvers';
	import { normalizeDiscNumber, getDiscTrackKey } from '$lib/player/queueHelpers';
	import { formatDuration } from '$lib/utils/formatting';
	import { playerStore } from '$lib/stores/player.svelte';
	import NowPlayingIndicator from '$lib/components/NowPlayingIndicator.svelte';
	import TrackPlayButton from '$lib/components/TrackPlayButton.svelte';
	import SampleButton from '$lib/components/discover/SampleButton.svelte';
	import TrackSourceButton from '$lib/components/TrackSourceButton.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/components/LocalFilesIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import LibraryFormatBadge from '$lib/components/library/LibraryFormatBadge.svelte';
	import LibraryTrackRow from '$lib/components/library/LibraryTrackRow.svelte';
	import { ChevronDown, TriangleAlert, TrendingUp, Check } from 'lucide-svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { LibraryFileMeta, DownloadTask, HeldImport } from '$lib/types';
	import TrackRequestButton from '$lib/components/downloads/TrackRequestButton.svelte';
	import TrackDownloadStatus from '$lib/components/downloads/TrackDownloadStatus.svelte';
	import HeldTrackReview from '$lib/components/downloads/HeldTrackReview.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { requestUpgradeTrack } from '$lib/queries/downloads/UpgradeQueries.svelte';
	import { toastStore } from '$lib/stores/toast';

	interface Props {
		album: AlbumBasicInfo;
		renderedTrackSections: RenderedTrackSection[];
		trackLinkMap: Map<string, YouTubeTrackLink>;
		jellyfinMatch: JellyfinAlbumMatch | null;
		localMatch: LocalAlbumMatch | null;
		navidromeMatch: NavidromeAlbumMatch | null;
		plexMatch: PlexAlbumMatch | null;
		jellyfinTrackMap: Map<string, JellyfinTrackInfo>;
		localTrackMap: Map<string, LocalTrackInfo>;
		navidromeTrackMap: Map<string, NavidromeTrackInfo>;
		plexTrackMap: Map<string, PlexTrackInfo>;
		jellyfinTracks: JellyfinTrackInfo[];
		localTracks: LocalTrackInfo[];
		navidromeTracks: NavidromeTrackInfo[];
		plexTracks: PlexTrackInfo[];
		trackLinks: YouTubeTrackLink[];
		youtubeEnabled: boolean;
		youtubeApiConfigured: boolean;
		jellyfinEnabled: boolean;
		localfilesEnabled: boolean;
		navidromeEnabled: boolean;
		plexEnabled: boolean;
		libraryTracksByRecording?: Map<string, LibraryFileMeta>;
		libraryTracksByPosition?: Map<string, LibraryFileMeta>;
		heldByRecording?: Map<string, HeldImport>;
		heldByPosition?: Map<string, HeldImport>;
		trackDownloadTasks?: Map<string, DownloadTask>;
		releaseGroupMbid?: string;
		onPlaySourceTrack: (
			source: 'jellyfin' | 'local' | 'navidrome' | 'plex',
			trackPosition: number,
			discNumber: number,
			title: string
		) => void;
		onTrackGenerated: (link: YouTubeTrackLink) => void;
		onQuotaUpdate: (q: YouTubeQuotaStatus) => void;
		getTrackContextMenuItems: (
			track: { position: number; disc_number?: number | null; title: string },
			resolvedLocal: LocalTrackInfo | null,
			resolvedJellyfin: JellyfinTrackInfo | null,
			resolvedNavidrome: NavidromeTrackInfo | null,
			resolvedPlex: PlexTrackInfo | null
		) => MenuItem[];
	}

	let {
		album,
		renderedTrackSections,
		trackLinkMap,
		jellyfinMatch,
		localMatch,
		navidromeMatch,
		plexMatch,
		jellyfinTrackMap,
		localTrackMap,
		navidromeTrackMap,
		plexTrackMap,
		jellyfinTracks,
		localTracks,
		navidromeTracks,
		plexTracks,
		trackLinks,
		youtubeEnabled,
		youtubeApiConfigured,
		jellyfinEnabled,
		localfilesEnabled,
		navidromeEnabled,
		plexEnabled,
		libraryTracksByRecording = new Map(),
		libraryTracksByPosition = new Map(),
		heldByRecording = new Map(),
		heldByPosition = new Map(),
		trackDownloadTasks = new Map(),
		releaseGroupMbid = '',
		onPlaySourceTrack,
		onTrackGenerated,
		onQuotaUpdate,
		getTrackContextMenuItems
	}: Props = $props();

	const expandedRows = new SvelteSet<number>();
	function toggleRow(idx: number) {
		if (expandedRows.has(idx)) expandedRows.delete(idx);
		else expandedRows.add(idx);
	}

	// rows whose "held · couldn't verify" review panel is open
	const heldOpen = new SvelteSet<number>();
	function toggleHeld(idx: number) {
		if (heldOpen.has(idx)) heldOpen.delete(idx);
		else heldOpen.add(idx);
	}

	// Per-track quality upgrade (admin/trusted, CollectionManagement D18): shown on
	// in-library tracks sitting below the cutoff while upgrades are on.
	const upgradeTrack = requestUpgradeTrack();
	const upgradeQueuedRecordings = new SvelteSet<string>();
	async function handleTrackUpgrade(
		recordingMbid: string,
		title: string,
		durationMs: number | null | undefined
	) {
		try {
			const result = await upgradeTrack.mutateAsync({
				recording_mbid: recordingMbid,
				artist_name: album.artist_name,
				track_title: title,
				album_title: album.title,
				duration_seconds: durationMs ? Math.round(durationMs / 1000) : null,
				release_group_mbid: releaseGroupMbid || album.musicbrainz_id,
				artist_mbid: album.artist_id
			});
			if (result.status === 'queued') {
				upgradeQueuedRecordings.add(recordingMbid);
				toastStore.show({ message: `Looking for a better copy of ${title}`, type: 'success' });
			} else {
				toastStore.show({ message: 'Already at or above the cutoff', type: 'info' });
			}
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : "Couldn't start that upgrade",
				type: 'error'
			});
		}
	}
</script>

<div class="rounded-2xl border border-base-content/8 bg-base-200/50 overflow-visible">
	<ul class="list">
		{#each renderedTrackSections as section (section.discNumber)}
			{#if renderedTrackSections.length > 1}
				<li class="list-row min-h-0 cursor-default px-3 sm:px-4 pt-4 pb-2">
					<div
						class="inline-flex items-center gap-2 rounded-full border border-base-content/10 bg-base-100/80 px-3 py-1 font-mono text-[0.62rem] font-bold uppercase tracking-[0.18em] text-base-content/60"
					>
						<span class="h-1.5 w-1.5 rounded-full bg-accent"></span>
						Disc {section.discNumber}
					</div>
				</li>
			{/if}
			{#each section.items as row (row.globalIndex)}
				{@const track = row.track}
				{@const trackDiscNumber = normalizeDiscNumber(track.disc_number)}
				{@const tl = trackLinkMap.get(getDiscTrackKey(track)) ?? null}
				{@const jellyfinTrack = resolveSourceTrack(
					trackDiscNumber,
					track.position,
					row.globalIndex,
					jellyfinTrackMap,
					jellyfinTracks
				)}
				{@const localTrack = resolveSourceTrack(
					trackDiscNumber,
					track.position,
					row.globalIndex,
					localTrackMap,
					localTracks
				)}
				{@const navidromeTrack = resolveSourceTrack(
					trackDiscNumber,
					track.position,
					row.globalIndex,
					navidromeTrackMap,
					navidromeTracks
				)}
				{@const plexTrack = resolveSourceTrack(
					trackDiscNumber,
					track.position,
					row.globalIndex,
					plexTrackMap,
					plexTracks
				)}
				{@const isCurrentlyPlaying =
					playerStore.nowPlaying?.albumId === album.musicbrainz_id &&
					(playerStore.currentQueueItem?.discNumber ?? 1) === trackDiscNumber &&
					playerStore.currentQueueItem?.trackNumber === track.position &&
					playerStore.isPlaying}
				{@const showJellyfinBtn = jellyfinEnabled && jellyfinMatch?.found}
				{@const showLocalBtn = localfilesEnabled && localMatch?.found}
				{@const showNavidromeBtn = navidromeEnabled && navidromeMatch?.found}
				{@const showPlexBtn = plexEnabled && plexMatch?.found}
				{@const hasAnySource =
					tl !== null ||
					jellyfinTrack !== null ||
					localTrack !== null ||
					navidromeTrack !== null ||
					plexTrack !== null}
				{@const showPreview = !hasAnySource}
				{@const libMeta =
					(track.recording_id ? libraryTracksByRecording.get(track.recording_id) : undefined) ??
					libraryTracksByPosition.get(
						getDiscTrackKey({ disc_number: trackDiscNumber, track_number: track.position })
					)}
				{@const heldMeta = libMeta
					? undefined
					: ((track.recording_id ? heldByRecording.get(track.recording_id) : undefined) ??
						heldByPosition.get(
							getDiscTrackKey({ disc_number: trackDiscNumber, track_number: track.position })
						))}
				{@const trackTask = track.recording_id
					? (trackDownloadTasks.get(track.recording_id) ?? null)
					: null}
				{@const showTrackDownload = !libMeta && !!trackTask}
				{@const showRequest =
					!libMeta &&
					!trackTask &&
					!heldMeta &&
					!!track.recording_id &&
					$integrationStore.download_client}
				{@const upgradeRecordingId = track.recording_id ?? libMeta?.recording_mbid ?? null}
				{@const showUpgrade =
					!!libMeta?.below_cutoff &&
					authStore.isTrusted &&
					!!upgradeRecordingId &&
					!trackTask &&
					$integrationStore.download_client}
				{@const hasActions =
					youtubeEnabled ||
					showPreview ||
					showJellyfinBtn ||
					showLocalBtn ||
					showNavidromeBtn ||
					showPlexBtn ||
					showRequest ||
					showTrackDownload ||
					!!heldMeta ||
					showUpgrade}
				<li
					class="list-row group transition-colors p-3 sm:p-4 {isCurrentlyPlaying
						? 'bg-accent/10'
						: 'hover:bg-base-200/60'}"
				>
					<div
						class="list-col-grow grid w-full min-w-0 grid-cols-[2rem_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1.5 sm:grid-cols-[2rem_minmax(0,1fr)_auto_max-content] sm:gap-x-4"
					>
						<div
							class="min-w-0 text-right font-mono text-sm tabular-nums {isCurrentlyPlaying
								? 'text-accent'
								: 'text-base-content/50'}"
						>
							{#if isCurrentlyPlaying}
								<div class="flex justify-end">
									<NowPlayingIndicator />
								</div>
							{:else}
								{track.position}
							{/if}
						</div>

						<div class="flex min-w-0 items-start gap-2">
							{#if libMeta}
								<div class="mt-1 shrink-0">
									<LibraryFormatBadge format={libMeta.file_format} size="badge-xs" />
								</div>
							{/if}

							<div class="min-w-0 flex-1">
								<div
									class="font-medium line-clamp-2 break-words {isCurrentlyPlaying
										? 'text-accent'
										: ''}"
								>
									{track.title}
								</div>
								{#if libMeta?.artist_name && libMeta.artist_name !== album.artist_name}
									<div class="truncate text-xs text-base-content/50">{libMeta.artist_name}</div>
								{/if}
							</div>
						</div>

						<div
							class="w-12 min-w-0 text-right font-mono text-xs tabular-nums text-base-content/50"
						>
							{formatDuration(track.length)}
						</div>

						{#if hasActions || libMeta}
							<div
								class="col-span-3 flex min-w-0 flex-wrap items-center justify-end gap-1.5 sm:col-span-1 sm:col-start-4 sm:row-start-1 sm:flex-nowrap"
							>
								{#if showTrackDownload && trackTask}
									<TrackDownloadStatus task={trackTask} />
								{/if}
								{#if heldMeta}
									<button
										class="btn btn-ghost btn-xs gap-1 text-warning"
										onclick={() => toggleHeld(row.globalIndex)}
										aria-expanded={heldOpen.has(row.globalIndex)}
										title="Downloaded but couldn't verify - review it"
									>
										<TriangleAlert class="h-3.5 w-3.5" /> held
									</button>
								{/if}
								{#if showRequest && track.recording_id}
									<TrackRequestButton
										recordingMbid={track.recording_id}
										trackTitle={track.title}
										artistName={album.artist_name}
										albumMbid={releaseGroupMbid || album.musicbrainz_id}
										albumTitle={album.title}
										durationSeconds={track.length ? Math.round(track.length / 1000) : null}
										artistMbid={album.artist_id}
									/>
								{/if}
								{#if showUpgrade && upgradeRecordingId}
									{#if upgradeQueuedRecordings.has(upgradeRecordingId)}
										<span class="btn btn-ghost btn-xs gap-1 pointer-events-none text-success">
											<Check class="h-3.5 w-3.5" /> queued
										</span>
									{:else}
										<button
											class="btn btn-ghost btn-xs gap-1 text-primary"
											disabled={upgradeTrack.isPending}
											onclick={() =>
												void handleTrackUpgrade(upgradeRecordingId, track.title, track.length)}
											title="Below your quality cutoff - find a better copy"
										>
											<TrendingUp class="h-3.5 w-3.5" /> upgrade
										</button>
									{/if}
								{/if}
								{#if showPreview}
									<SampleButton
										sampleKey={`track:${album.artist_name}|${track.title}`}
										artist={album.artist_name}
										title={track.title}
										kind="track"
										size="xs"
										albumMbid={album.musicbrainz_id}
										artistMbid={album.artist_id}
										coverUrl={album.cover_url ?? null}
									/>
								{/if}

								{#if youtubeEnabled}
									<TrackPlayButton
										trackNumber={track.position}
										discNumber={trackDiscNumber}
										trackName={track.title}
										trackLink={tl}
										allTrackLinks={trackLinks}
										albumId={album.musicbrainz_id}
										albumName={album.title}
										artistName={album.artist_name}
										coverUrl={album.cover_url ?? null}
										artistId={album.artist_id}
										apiConfigured={youtubeApiConfigured}
										onGenerated={onTrackGenerated}
										{onQuotaUpdate}
									/>
								{/if}

								{#if showJellyfinBtn}
									<TrackSourceButton
										available={jellyfinTrack !== null}
										sourceColor="rgb(var(--brand-jellyfin))"
										onclick={() =>
											onPlaySourceTrack('jellyfin', track.position, trackDiscNumber, track.title)}
										ariaLabel={jellyfinTrack ? 'Play on Jellyfin' : 'Not available on Jellyfin'}
									>
										{#snippet icon()}
											<JellyfinIcon class="h-4 w-4" />
										{/snippet}
									</TrackSourceButton>
								{/if}

								{#if showLocalBtn}
									<TrackSourceButton
										available={localTrack !== null}
										sourceColor="rgb(var(--brand-localfiles))"
										onclick={() =>
											onPlaySourceTrack('local', track.position, trackDiscNumber, track.title)}
										ariaLabel={localTrack ? 'Play local file' : 'Not available locally'}
									>
										{#snippet icon()}
											<LocalFilesIcon class="h-4 w-4" />
										{/snippet}
									</TrackSourceButton>
								{/if}

								{#if showNavidromeBtn}
									<TrackSourceButton
										available={navidromeTrack !== null}
										sourceColor="rgb(var(--brand-navidrome))"
										onclick={() =>
											onPlaySourceTrack('navidrome', track.position, trackDiscNumber, track.title)}
										ariaLabel={navidromeTrack ? 'Play on Navidrome' : 'Not available on Navidrome'}
									>
										{#snippet icon()}
											<NavidromeIcon class="h-4 w-4" />
										{/snippet}
									</TrackSourceButton>
								{/if}

								{#if showPlexBtn}
									<TrackSourceButton
										available={plexTrack !== null}
										sourceColor="rgb(var(--brand-plex))"
										onclick={() =>
											onPlaySourceTrack('plex', track.position, trackDiscNumber, track.title)}
										ariaLabel={plexTrack ? 'Play on Plex' : 'Not available on Plex'}
									>
										{#snippet icon()}
											<PlexIcon class="h-4 w-4" />
										{/snippet}
									</TrackSourceButton>
								{/if}

								{#if hasActions}
									<div>
										<ContextMenu
											items={getTrackContextMenuItems(
												track,
												localTrack,
												jellyfinTrack,
												navidromeTrack,
												plexTrack
											)}
											position="end"
											size="xs"
										/>
									</div>
								{/if}

								{#if libMeta}
									<button
										class="btn btn-ghost btn-xs btn-circle shrink-0"
										onclick={() => toggleRow(row.globalIndex)}
										aria-label={expandedRows.has(row.globalIndex)
											? 'Hide file details'
											: 'Show file details'}
									>
										<ChevronDown
											class="h-4 w-4 transition-transform {expandedRows.has(row.globalIndex)
												? 'rotate-180'
												: ''}"
										/>
									</button>
								{/if}
							</div>
						{/if}
					</div>
				</li>
				{#if libMeta && expandedRows.has(row.globalIndex)}
					<li class="list-row p-0">
						<LibraryTrackRow meta={libMeta} {releaseGroupMbid} />
					</li>
				{/if}
				{#if heldMeta && heldOpen.has(row.globalIndex)}
					<li class="list-row p-0">
						<div class="w-full px-3 pb-3 sm:px-4">
							<HeldTrackReview held={heldMeta} />
						</div>
					</li>
				{/if}
			{/each}
		{/each}
	</ul>
</div>
