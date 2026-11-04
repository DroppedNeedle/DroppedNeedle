<script lang="ts">
	import type { AlbumBasicInfo, AlbumTracksInfo } from '$lib/features/album/queries/types';
	import type { DownloadTask, HeldFile } from '$lib/features/downloads/queries/types';
	import type { AlbumView } from '$lib/features/library/queries/types';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AlbumDownloadStatus from '$lib/features/downloads/components/AlbumDownloadStatus.svelte';
	import { formatTotalDuration } from '$lib/utils/formatting';
	import type { Snippet } from 'svelte';
	import {
		Check,
		Clock,
		Disc3,
		FolderSync,
		Play,
		Plus,
		RefreshCw,
		Square,
		Trash2,
		TrendingUp,
		TriangleAlert
	} from 'lucide-svelte';
	import { createRescanAlbumMutation } from '$lib/features/library/queries/LibraryMutations.svelte';
	import { createRequestUpgradeAlbumMutation } from '$lib/features/downloads/queries/UpgradeQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';
	import EditionPlate from '$lib/features/library/components/edition/EditionPlate.svelte';
	import NoFilesBadge from '$lib/features/library/components/NoFilesBadge.svelte';
	import {
		editionSheet,
		type EditionRequestPick
	} from '$lib/features/library/components/edition/editionSheetState.svelte';
	import AlbumDownloadButton from './AlbumDownloadButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ContextMenu, { type MenuItem } from '$lib/ui/ContextMenu.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import AlbumHero, { type AlbumFact } from './AlbumHero.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';

	interface Props {
		album: AlbumBasicInfo;
		tracksInfo: AlbumTracksInfo | null;
		loadingTracks: boolean;
		inLibrary: boolean;
		isRequested: boolean;
		requesting: boolean;
		refreshing: boolean;
		headerDownloadTask: DownloadTask | null;
		heldFiles?: HeldFile[];
		downloadClientConfigured: boolean;

		libraryInLibrary?: boolean;
		libraryTrackCount?: number;
		mbTrackCount?: number;
		libraryBelowCutoff?: boolean;
		/** The current edition's tracks a file sits on, out of all of them, once its plan is read. */
		editionCounts?: { kept: number; total: number } | null;
		releaseGroupMbid?: string;
		localCopies?: AlbumView[];
		/** The edition a request gets, for a curator on an album the library does not hold. */
		editionPick?: EditionRequestPick | null;
		downloadAllowed?: boolean;
		/** The caller's rating of the local album, 0 when not rated. */
		rating?: number;
		/** Rate the local album, or clear it with 0. The stars show only with this. */
		onrate?: (rating: number) => void;
		/** Whether the caller favorited the local album. */
		favorite?: boolean;
		/** Heart or unheart the local album. The heart shows beside the stars. */
		onfavorite?: (favorite: boolean) => void;
		/** Plays the album from its first source with tracks; no Play without it. */
		onplay?: () => void;
		/** The formats the files are in, e.g. "FLAC". */
		formats?: string | null;
		back?: Snippet;
		onrequest: () => void;
		ondelete: () => void;
		onrefresh: () => void;
		onartistclick: () => void;
	}

	let {
		album,
		tracksInfo,
		loadingTracks,
		inLibrary,
		isRequested,
		requesting,
		refreshing,
		headerDownloadTask,
		heldFiles = [],
		downloadClientConfigured,
		libraryInLibrary = false,
		libraryTrackCount = 0,
		mbTrackCount = 0,
		libraryBelowCutoff = false,
		editionCounts = null,
		releaseGroupMbid = '',
		localCopies = [],
		editionPick = null,
		downloadAllowed = true,
		rating = 0,
		onrate,
		favorite = false,
		onfavorite,
		onplay,
		formats = null,
		back,
		onrequest,
		ondelete,
		onrefresh,
		onartistclick
	}: Props = $props();

	const headerSampling = $derived(
		deckSampler.activeKey === album?.musicbrainz_id && deckSampler.status !== 'idle'
	);

	function toggleHeaderSample() {
		if (headerSampling) {
			deckSampler.stop();
			return;
		}
		deckSampler.start(album.musicbrainz_id, album.artist_name, album.title, {
			albumMbid: album.musicbrainz_id,
			artistMbid: album.artist_id,
			coverUrl: album.cover_url
		});
	}

	const rescan = createRescanAlbumMutation();
	// With the edition's plan read, "In Library" counts the edition's tracks a
	// file sits on: a file that sits on none reads "Unmatched files", never
	// owned. Before the plan is there, held files are counted.
	const libraryComplete = $derived(
		editionCounts
			? libraryInLibrary && editionCounts.kept >= editionCounts.total
			: libraryInLibrary && mbTrackCount > 0 && libraryTrackCount >= mbTrackCount
	);
	const libraryUnmatchedOnly = $derived(
		!!editionCounts && libraryInLibrary && libraryTrackCount > 0 && editionCounts.kept === 0
	);

	async function handleRescan() {
		try {
			await rescan.mutateAsync(releaseGroupMbid);
			toastStore.show({ message: 'Rescan started.', type: 'success' });
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Rescan failed',
				type: 'error'
			});
		}
	}

	// Album quality upgrade (admin/trusted): fetch a
	// better copy of everything below the cutoff; replace is strictly-better-only.
	const upgrade = createRequestUpgradeAlbumMutation();
	let upgradeQueued = $state(false);
	async function handleUpgrade() {
		try {
			const result = await upgrade.mutateAsync({
				release_group_mbid: releaseGroupMbid || album.musicbrainz_id,
				artist_name: album.artist_name,
				album_title: album.title,
				year: album.year,
				artist_mbid: album.artist_id
			});
			if (result.status === 'queued') {
				upgradeQueued = true;
				toastStore.show({ message: 'Looking for a better copy of this album.', type: 'success' });
			} else {
				toastStore.show({ message: 'Already at or above the cutoff.', type: 'info' });
			}
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Upgrade failed',
				type: 'error'
			});
		}
	}

	// SvelteKit reuses this component instance across album navigations, so the
	// per-album "queued" button state must reset when the album changes
	$effect(() => {
		void (releaseGroupMbid || album.musicbrainz_id);
		upgradeQueued = false;
	});

	// The edition plate stays while its sheet is open, even when a choice
	// moved the copy off this page.
	const showPlate = $derived(localCopies.length > 0 || editionSheet.open || !!editionPick);

	const owned = $derived(inLibrary || libraryInLibrary);
	const canRemove = $derived(authStore.isAdmin && (owned || isRequested));

	const facts = $derived.by(() => {
		const list: AlbumFact[] = [];
		if (tracksInfo?.label) list.push({ label: 'Label', value: tracksInfo.label });
		if (tracksInfo?.country) list.push({ label: 'Country', value: tracksInfo.country });
		if (album.release_date) list.push({ label: 'Released', value: album.release_date });
		if (tracksInfo?.barcode) list.push({ label: 'Barcode', value: tracksInfo.barcode });
		return list;
	});

	const menuItems = $derived.by(() => {
		const items: MenuItem[] = [];
		if (inLibrary || isRequested) {
			items.push({
				label: refreshing ? 'Refreshing...' : 'Refresh album status',
				icon: RefreshCw,
				disabled: refreshing,
				onclick: onrefresh
			});
		}
		if (libraryInLibrary && authStore.isAdmin) {
			items.push({
				label: rescan.isPending ? 'Rescanning...' : 'Rescan',
				icon: FolderSync,
				disabled: rescan.isPending,
				onclick: () => void handleRescan()
			});
		}
		if (libraryInLibrary && authStore.isTrusted && libraryBelowCutoff && downloadClientConfigured) {
			items.push({
				label: upgradeQueued ? 'Upgrade queued' : 'Upgrade quality',
				icon: TrendingUp,
				disabled: upgrade.isPending || upgradeQueued,
				onclick: () => void handleUpgrade()
			});
		}
		if (canRemove) {
			items.push({ label: 'Remove', icon: Trash2, className: 'text-error', onclick: ondelete });
		}
		return items;
	});
</script>

<AlbumHero
	eyebrow={album.type || 'Album'}
	title={album.title}
	note={album.disambiguation}
	{facts}
	{back}
>
	{#snippet cover()}
		<AlbumImage
			mbid={album.musicbrainz_id}
			customUrl={album.cover_url}
			remoteUrl={album.album_thumb_url ?? null}
			alt={`Album cover for ${album.title} by ${album.artist_name}`}
			size="full"
			requestSize={500}
			priority
			rounded="none"
			className="aspect-square w-full"
		/>
	{/snippet}

	{#snippet meta()}
		<button type="button" onclick={onartistclick} class="font-bold hover:underline">
			{album.artist_name}
		</button>
		{#if album.year}
			<span class="opacity-50" aria-hidden="true">•</span>
			<span title="First released">{album.year}</span>
		{/if}
		{#if tracksInfo && tracksInfo.total_tracks > 0}
			<span class="opacity-50" aria-hidden="true">•</span>
			<span>{tracksInfo.total_tracks} {tracksInfo.total_tracks === 1 ? 'track' : 'tracks'}</span>
		{:else if loadingTracks}
			<span class="opacity-50" aria-hidden="true">•</span>
			<span class="skeleton inline-block h-4 w-16"></span>
		{/if}
		{#if tracksInfo?.total_length}
			<span class="opacity-50" aria-hidden="true">•</span>
			<span>{formatTotalDuration(tracksInfo.total_length)}</span>
		{/if}
		{#if formats}
			<span class="opacity-50" aria-hidden="true">•</span>
			<span>{formats}</span>
		{/if}
		{#if onrate}
			<span class="flex items-center">
				<RatingStars size="md" value={rating} label={album.title} onchange={onrate} />
				{#if onfavorite}
					<FavoriteHeart size="md" {favorite} onchange={onfavorite} />
				{/if}
			</span>
		{/if}
	{/snippet}

	{#snippet details()}
		{#if localCopies.length > 0 && localCopies.every((copy) => copy.track_count === 0)}
			<NoFilesBadge />
		{/if}
		{#if showPlate}
			<EditionPlate
				copies={localCopies}
				groupMbid={releaseGroupMbid || album.musicbrainz_id}
				request={editionPick ?? undefined}
			/>
		{/if}
	{/snippet}

	{#snippet actions()}
		{#if onplay}
			<Button variant="primary" icon={Play} class="min-w-28 [&_svg]:fill-current" onclick={onplay}>
				Play
			</Button>
		{/if}
		{#if owned}
			{#if libraryUnmatchedOnly}
				<StatusChip tone="error" size="md" icon={TriangleAlert}>Unmatched files only</StatusChip>
			{:else if libraryComplete || !editionCounts}
				<StatusChip size="md" icon={Check}>In Library</StatusChip>
			{:else}
				<StatusChip tone="warning" size="md" icon={Check}>
					In Library · {editionCounts.kept}/{editionCounts.total}
				</StatusChip>
			{/if}
			<AlbumDownloadButton
				albumId={localCopies[0]?.id || null}
				mbid={localCopies[0]?.id ? null : releaseGroupMbid || album.musicbrainz_id || null}
				totalSizeBytes={localCopies[0]?.total_size_bytes ?? null}
				trackCount={libraryTrackCount}
				{downloadAllowed}
			/>
		{:else if isRequested}
			{#if !headerDownloadTask}
				<StatusChip tone="warning" size="md" icon={Clock}>Requested</StatusChip>
			{/if}
		{:else}
			<Button
				variant={onplay ? 'outline' : 'primary'}
				icon={Plus}
				loading={requesting}
				onclick={() => onrequest()}
			>
				{requesting ? 'Requesting...' : 'Add to Library'}
			</Button>
		{/if}
		{#if !inLibrary}
			<Button
				variant="outline"
				active={headerSampling}
				onclick={toggleHeaderSample}
				title="Hear 30-second samples of this album before you grab it"
			>
				{#if headerSampling && deckSampler.status === 'loading'}
					<span class="loading loading-spinner loading-sm"></span>
				{:else if headerSampling}
					<Square class="size-4" fill="currentColor" />
				{:else}
					<Disc3 class="size-4" />
				{/if}
				{headerSampling ? 'Stop sample' : 'Sample'}
			</Button>
		{/if}
		{#if menuItems.length > 0}
			<ContextMenu items={menuItems} trigger="button" label="More album actions" />
		{/if}
	{/snippet}

	{#snippet status()}
		{#if headerDownloadTask}
			<AlbumDownloadStatus task={headerDownloadTask} {heldFiles} />
		{/if}
	{/snippet}
</AlbumHero>
