<script lang="ts">
	import type {
		AlbumBasicInfo,
		LocalAlbumMatch,
		LocalTrackInfo,
		LibraryStatusTrack,
		HeldFileInEntry
	} from '$lib/features/album/queries/types';
	import type { YouTubeTrackLink, YouTubeQuotaStatus } from '$lib/features/youtube/queries/types';
	import type { RemoteMatch, RemoteTrack } from '$lib/features/remote-library/queries/types';
	import type { MenuItem } from '$lib/ui/ContextMenu.svelte';
	import type { RenderedTrackSection } from './albumTrackResolvers';
	import type { FetchedEditionRow } from '../state/albumPageState.svelte';
	import { resolveSourceTrack } from './albumTrackResolvers';
	import { normalizeDiscNumber, getDiscTrackKey } from '$lib/features/player/playback/queueHelpers';
	import { formatBytes, formatDuration } from '$lib/utils/formatting';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import NowPlayingIndicator from '$lib/features/player/components/NowPlayingIndicator.svelte';
	import TrackPlayButton from '$lib/features/album/components/TrackPlayButton.svelte';
	import SampleButton from '$lib/features/discover/components/SampleButton.svelte';
	import TrackSourceButton from '$lib/features/album/components/TrackSourceButton.svelte';
	import ContextMenu from '$lib/ui/ContextMenu.svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import LocalFilesIcon from '$lib/media/icons/LocalFilesIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import LibraryFormatBadge from '$lib/features/library/components/LibraryFormatBadge.svelte';
	import LibraryTrackRow from '$lib/features/library/components/LibraryTrackRow.svelte';
	import { ChevronDown, ChevronUp, TriangleAlert, TrendingUp, Check } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';
	import TrackRequestButton from '$lib/features/downloads/components/TrackRequestButton.svelte';
	import TrackDownloadStatus from '$lib/features/downloads/components/TrackDownloadStatus.svelte';
	import HeldFileRow from '$lib/features/downloads/components/HeldFileRow.svelte';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { createRequestUpgradeTrackMutation } from '$lib/features/downloads/queries/UpgradeQueries.svelte';
	import { toastStore } from '$lib/stores/toast';

	const integrations = useIntegrationStatus();

	interface Props {
		album: AlbumBasicInfo;
		renderedTrackSections: RenderedTrackSection[];
		trackLinkMap: Map<string, YouTubeTrackLink>;
		jellyfinMatch: RemoteMatch | null;
		localMatch: LocalAlbumMatch | null;
		navidromeMatch: RemoteMatch | null;
		plexMatch: RemoteMatch | null;
		jellyfinTrackMap: Map<string, RemoteTrack>;
		localTrackMap: Map<string, LocalTrackInfo>;
		navidromeTrackMap: Map<string, RemoteTrack>;
		plexTrackMap: Map<string, RemoteTrack>;
		jellyfinTracks: RemoteTrack[];
		localTracks: LocalTrackInfo[];
		navidromeTracks: RemoteTrack[];
		plexTracks: RemoteTrack[];
		trackLinks: YouTubeTrackLink[];
		youtubeEnabled: boolean;
		youtubeApiConfigured: boolean;
		jellyfinEnabled: boolean;
		localfilesEnabled: boolean;
		navidromeEnabled: boolean;
		plexEnabled: boolean;
		libraryTracksByRecording?: Map<string, LibraryStatusTrack>;
		libraryTracksByPosition?: Map<string, LibraryStatusTrack>;
		heldByRecording?: Map<string, HeldFileInEntry>;
		heldByPosition?: Map<string, HeldFileInEntry>;
		trackDownloadTasks?: Map<string, DownloadTask>;
		/** The current edition's plan by disc and position; it decides which file holds a track. */
		editionRows?: Map<string, FetchedEditionRow> | null;
		releaseGroupMbid?: string;
		releaseMbid?: string | null;
		/** The caller's rating of each local track they rated, by track id. */
		trackRatings?: Record<string, number>;
		/** Rate a local track, or clear it with 0. Rows with a file show stars only with this. */
		onRateTrack?: (trackId: string, rating: number) => void;
		/** Heart or unheart a local track. Rows with a file show the heart beside the stars. */
		onFavoriteTrack?: (trackId: string, favorite: boolean) => void;
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
			resolvedJellyfin: RemoteTrack | null,
			resolvedNavidrome: RemoteTrack | null,
			resolvedPlex: RemoteTrack | null
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
		editionRows = null,
		releaseGroupMbid = '',
		releaseMbid = null,
		trackRatings = {},
		onRateTrack,
		onFavoriteTrack,
		onPlaySourceTrack,
		onTrackGenerated,
		onQuotaUpdate,
		getTrackContextMenuItems
	}: Props = $props();

	// Rows line their titles up behind a format column once any track has a file.
	const reserveFormat = $derived(
		libraryTracksByPosition.size > 0 ||
			libraryTracksByRecording.size > 0 ||
			(editionRows?.size ?? 0) > 0
	);

	const expandedRows = new SvelteSet<number>();
	function toggleRow(idx: number) {
		if (expandedRows.has(idx)) expandedRows.delete(idx);
		else expandedRows.add(idx);
	}

	// rows whose held review panel is open
	const heldOpen = new SvelteSet<number>();
	function toggleHeld(idx: number) {
		if (heldOpen.has(idx)) heldOpen.delete(idx);
		else heldOpen.add(idx);
	}

	// Per-track quality upgrade (admin/trusted): shown on
	// in-library tracks sitting below the cutoff while upgrades are on.
	const upgradeTrack = createRequestUpgradeTrackMutation();
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

<ul class="divide-y divide-paper-border/70">
	{#each renderedTrackSections as section (section.discNumber)}
		{#if renderedTrackSections.length > 1}
			<li
				class="bg-base-300/60 px-3 pt-3 pb-1.5 font-mono text-xs font-bold tracking-[0.18em] text-muted uppercase sm:px-4"
			>
				Disc {section.discNumber}
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
			{@const editionRow =
				editionRows?.get(
					getDiscTrackKey({ disc_number: trackDiscNumber, track_number: track.position })
				) ?? null}
			{@const libMeta = editionRow
				? (editionRow.file ?? undefined)
				: ((track.recording_id ? libraryTracksByRecording.get(track.recording_id) : undefined) ??
					libraryTracksByPosition.get(
						getDiscTrackKey({ disc_number: trackDiscNumber, track_number: track.position })
					))}
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
			{@const showRequest = !libMeta && !trackTask && !heldMeta && !!track.recording_id}
			{@const upgradeRecordingId = track.recording_id ?? libMeta?.recording_mbid ?? null}
			{@const showUpgrade =
				!!libMeta?.below_cutoff &&
				authStore.isTrusted &&
				!!upgradeRecordingId &&
				!trackTask &&
				integrations.current.download_client}
			{@const rateTrack = libMeta && onRateTrack ? onRateTrack : null}
			<li
				class="px-2 py-0.5 transition-colors hover:[--dn-quiet-ink:currentColor] focus-within:[--dn-quiet-ink:currentColor] sm:px-4 {isCurrentlyPlaying
					? 'bg-selected'
					: 'hover:bg-base-content/4'}"
			>
				<!-- One line from 640 up. Narrower, the format, length and status
				     drop to a line under the title so the title keeps its room. -->
				<div class="flex min-h-9 w-full flex-wrap items-center gap-x-2 sm:flex-nowrap sm:gap-x-3">
					<div
						class="w-6 shrink-0 text-center text-sm tabular-nums {isCurrentlyPlaying
							? 'text-primary'
							: 'text-muted'}"
					>
						{#if isCurrentlyPlaying}
							<NowPlayingIndicator color="bg-primary" />
						{:else}
							{track.position}
						{/if}
					</div>

					<div class="order-2 min-w-0 flex-1 py-1">
						<div class="text-[0.9375rem] leading-snug font-semibold break-words">
							{track.title}
						</div>
						{#if libMeta?.artist_name && libMeta.artist_name !== album.artist_name}
							<div class="text-xs text-muted">{libMeta.artist_name}</div>
						{/if}
						{#if isCurrentlyPlaying}
							<div class="text-xs font-semibold text-primary" aria-hidden="true">Now playing</div>
						{/if}
					</div>

					<div
						class="order-last flex basis-full items-center gap-2 pb-1 pl-8 empty:hidden sm:contents"
					>
						{#if libMeta || reserveFormat}
							<div class="shrink-0 sm:order-1 sm:w-11">
								{#if libMeta}
									<LibraryFormatBadge format={libMeta.format} size="badge-xs" />
								{/if}
							</div>
						{/if}
						{#if editionRow?.missing}
							<span class="shrink-0 sm:order-3">
								{#if editionRow.fetchState === 'waiting' || editionRow.fetchState === 'fetching'}
									<span class="badge badge-sm badge-info badge-outline">Fetching</span>
								{:else if editionRow.fetchState === 'failed'}
									<span class="badge badge-sm badge-error badge-outline">Failed</span>
								{:else if editionRow.fetchState === 'held'}
									<span class="badge badge-sm badge-warning badge-outline">Held</span>
								{:else}
									<span class="badge badge-sm badge-warning badge-outline">Missing</span>
								{/if}
							</span>
						{/if}
						<div
							class="shrink-0 text-xs text-muted tabular-nums sm:order-5 sm:w-10 sm:text-right sm:text-sm sm:text-base-content"
						>
							{formatDuration(editionRow ? editionRow.lengthMs : track.length)}
						</div>
					</div>
					{#if rateTrack && libMeta}
						<div class="order-4 hidden shrink-0 items-center md:flex">
							<RatingStars
								quiet
								value={trackRatings[libMeta.id] ?? 0}
								label={track.title}
								onchange={(rating) => rateTrack(libMeta.id, rating)}
							/>
							{#if onFavoriteTrack}
								<FavoriteHeart
									quiet
									favorite={libMeta.favorite ?? false}
									onchange={(favorite) => onFavoriteTrack(libMeta.id, favorite)}
								/>
							{/if}
						</div>
					{/if}
					<div class="order-6 hidden w-16 shrink-0 text-right text-sm tabular-nums sm:block">
						{localTrack && localTrack.size_bytes > 0 ? formatBytes(localTrack.size_bytes) : '—'}
					</div>

					{#if youtubeEnabled || showPreview || showJellyfinBtn || showLocalBtn || showNavidromeBtn || showPlexBtn || showRequest || showTrackDownload || heldMeta || showUpgrade}
						<div class="order-7 ml-auto flex shrink-0 items-center gap-1">
							{#if showTrackDownload && trackTask}
								<TrackDownloadStatus task={trackTask} />
							{/if}
							{#if heldMeta}
								<Button
									variant="ghost"
									size="xs"
									onclick={() => toggleHeld(row.globalIndex)}
									aria-expanded={heldOpen.has(row.globalIndex)}
									title={heldMeta.file.reason_text}
								>
									<TriangleAlert class="size-3.5 text-warning" aria-hidden="true" /> held
								</Button>
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
									{releaseMbid}
								/>
							{/if}
							{#if showUpgrade && upgradeRecordingId}
								{#if upgradeQueuedRecordings.has(upgradeRecordingId)}
									<span
										class="inline-flex h-6 items-center gap-1 px-2 text-xs font-semibold text-success"
									>
										<Check class="h-3.5 w-3.5" /> queued
									</span>
								{:else}
									<Button
										variant="ghost"
										size="xs"
										icon={TrendingUp}
										disabled={upgradeTrack.isPending}
										onclick={() =>
											void handleTrackUpgrade(upgradeRecordingId, track.title, track.length)}
										title="Below your quality cutoff - find a better copy"
									>
										upgrade
									</Button>
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
									sourceColor="var(--color-accent)"
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

							<div>
								<ContextMenu
									size="xs"
									items={getTrackContextMenuItems(
										track,
										localTrack,
										jellyfinTrack,
										navidromeTrack,
										plexTrack
									)}
									position="end"
								/>
							</div>
						</div>
					{/if}

					{#if libMeta}
						<IconButton
							size="xs"
							icon={expandedRows.has(row.globalIndex) ? ChevronUp : ChevronDown}
							label={expandedRows.has(row.globalIndex) ? 'Hide file details' : 'Show file details'}
							class="order-8 shrink-0"
							onclick={() => toggleRow(row.globalIndex)}
						/>
					{/if}
				</div>
				{#if editionRow?.yourVersionMs || editionRow?.note}
					<!-- Under the title, on a line of its own so a phone keeps it readable. -->
					<div
						class="flex flex-col items-start pb-1 pl-8 text-xs {reserveFormat
							? 'sm:pl-[5.75rem]'
							: 'sm:pl-9'}"
					>
						{#if editionRow?.yourVersionMs}
							<span class="text-muted">Your version {formatDuration(editionRow.yourVersionMs)}</span
							>
						{/if}
						{#if editionRow?.note}
							<span class="text-info">{editionRow.note}</span>
						{/if}
					</div>
				{/if}
			</li>
			{#if libMeta && expandedRows.has(row.globalIndex)}
				<li>
					{#if rateTrack || localTrack}
						<!-- What the row leaves out on a narrow screen. -->
						<div class="flex flex-wrap items-center gap-x-4 gap-y-1 px-4 pt-2 text-xs md:hidden">
							{#if localTrack && localTrack.size_bytes > 0}
								<span class="sm:hidden"
									><span class="text-muted">Size:</span> {formatBytes(localTrack.size_bytes)}</span
								>
							{/if}
							{#if rateTrack}
								<div class="flex items-center">
									<RatingStars
										value={trackRatings[libMeta.id] ?? 0}
										label={track.title}
										onchange={(rating) => rateTrack(libMeta.id, rating)}
									/>
									{#if onFavoriteTrack}
										<FavoriteHeart
											favorite={libMeta.favorite ?? false}
											onchange={(favorite) => onFavoriteTrack(libMeta.id, favorite)}
										/>
									{/if}
								</div>
							{/if}
						</div>
					{/if}
					<LibraryTrackRow meta={libMeta} />
				</li>
			{/if}
			{#if heldMeta && heldOpen.has(row.globalIndex)}
				<li>
					<div class="w-full px-3 pb-3 sm:px-4">
						<HeldFileRow entry={heldMeta.entry} file={heldMeta.file} showTitle={false} showExpiry />
					</div>
				</li>
			{/if}
		{/each}
	{/each}
</ul>
