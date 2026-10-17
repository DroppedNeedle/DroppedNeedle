<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import type {
		AlbumBasicInfo,
		AlbumTracksInfo,
		DownloadTask,
		HeldImport,
		LibraryAlbumSummary
	} from '$lib/types';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import HeroBackdrop from '$lib/components/HeroBackdrop.svelte';
	import AlbumDownloadStatus from '$lib/components/downloads/AlbumDownloadStatus.svelte';
	import { formatTotalDuration } from '$lib/utils/formatting';
	import {
		Check,
		Trash2,
		Clock,
		Plus,
		RefreshCw,
		Disc3,
		Square,
		TrendingUp,
		TriangleAlert
	} from 'lucide-svelte';
	import { rescanAlbum } from '$lib/queries/library/LibraryMutations.svelte';
	import { requestUpgradeAlbum } from '$lib/queries/downloads/UpgradeQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { deckSampler } from '$lib/stores/deckSampler.svelte';
	import EditionPlate from '$lib/components/library/edition/EditionPlate.svelte';
	import { editionSheet } from '$lib/components/library/edition/editionSheetState.svelte';
	import AlbumDownloadButton from './AlbumDownloadButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		album: AlbumBasicInfo;
		tracksInfo: AlbumTracksInfo | null;
		loadingTracks: boolean;
		inLibrary: boolean;
		isRequested: boolean;
		requesting: boolean;
		refreshing: boolean;
		headerDownloadTask: DownloadTask | null;
		managementHeld?: HeldImport[];
		downloadClientConfigured: boolean;

		libraryInLibrary?: boolean;
		libraryTrackCount?: number;
		mbTrackCount?: number;
		libraryBelowCutoff?: boolean;
		/** The current edition's tracks a file sits on, out of all of them, once its plan is read. */
		editionCounts?: { kept: number; total: number } | null;
		releaseGroupMbid?: string;
		localCopies?: LibraryAlbumSummary[];
		downloadAllowed?: boolean;
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
		managementHeld = [],
		downloadClientConfigured,
		libraryInLibrary = false,
		libraryTrackCount = 0,
		mbTrackCount = 0,
		libraryBelowCutoff = false,
		editionCounts = null,
		releaseGroupMbid = '',
		localCopies = [],
		downloadAllowed = true,
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

	const rescan = rescanAlbum();
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
	const upgrade = requestUpgradeAlbum();
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
	const showPlate = $derived(localCopies.length > 0 || editionSheet.open);

	let backdropUrl = $derived(
		album.musicbrainz_id
			? coverSrc('release-group', album.musicbrainz_id, 500)
			: album.cover_url || album.album_thumb_url || null
	);
</script>

<div class="album-hero group relative rounded-2xl transition-all duration-500">
	<!-- The clip lives on this backdrop-only layer, NOT the card: overflow-hidden on
	     the card would trap the Edition dropdown menu inside it. -->
	<div class="absolute inset-0 overflow-hidden rounded-2xl">
		<HeroBackdrop
			imageUrl={backdropUrl}
			opacity={0.1}
			hoverOpacity={0.15}
			blur={3}
			hoverBlur={2}
			position="full"
		/>
	</div>

	<div class="relative z-10 flex flex-col lg:flex-row gap-6 lg:gap-8 p-4 sm:p-6 lg:p-8">
		{#if (inLibrary || isRequested) && downloadClientConfigured}
			<IconButton
				size="sm"
				icon={RefreshCw}
				label="Refresh album status"
				class="absolute top-3 right-3 z-20"
				loading={refreshing}
				onclick={onrefresh}
			/>
		{/if}
		<div class="w-full lg:w-64 xl:w-80 flex-shrink-0">
			<AlbumImage
				mbid={album.musicbrainz_id}
				customUrl={album.cover_url}
				remoteUrl={album.album_thumb_url ?? null}
				alt={album.title}
				size="hero"
				lazy={false}
				rounded="xl"
				className="w-full aspect-square shadow-2xl"
			/>
		</div>

		<div class="flex-1 flex flex-col lg:justify-end space-y-4">
			<div class="text-xs sm:text-sm font-semibold uppercase tracking-wider opacity-70">
				{album.type || 'Album'}
			</div>

			<h1 class="hero-title text-3xl sm:text-4xl lg:text-5xl xl:text-6xl font-bold leading-tight">
				{album.title}
			</h1>

			{#if album.disambiguation}
				<p class="text-sm opacity-60 italic">({album.disambiguation})</p>
			{/if}

			<div class="flex flex-wrap items-center gap-2 text-sm">
				<button
					type="button"
					onclick={onartistclick}
					class="font-semibold hover:underline cursor-pointer"
				>
					{album.artist_name}
				</button>

				{#if album.year}
					<span class="opacity-50">•</span>
					<span title="First released">{album.year}</span>
				{/if}

				{#if tracksInfo && tracksInfo.total_tracks > 0}
					<span class="opacity-50">•</span>
					<span>{tracksInfo.total_tracks} {tracksInfo.total_tracks === 1 ? 'track' : 'tracks'}</span
					>
				{:else if loadingTracks}
					<span class="opacity-50">•</span>
					<span class="skeleton w-16 h-4 inline-block"></span>
				{/if}

				{#if tracksInfo?.total_length}
					<span class="opacity-50">•</span>
					<span>{formatTotalDuration(tracksInfo.total_length)}</span>
				{/if}
			</div>

			{#if showPlate}
				<EditionPlate copies={localCopies} groupMbid={releaseGroupMbid || album.musicbrainz_id} />
			{/if}

			{#if libraryInLibrary}
				<div class="flex flex-wrap items-center gap-2">
					<span
						class="badge badge-sm gap-1 {libraryComplete
							? 'badge-success'
							: libraryUnmatchedOnly
								? 'badge-error'
								: 'badge-warning'}"
					>
						<Disc3 class="h-3.5 w-3.5" />
						{libraryComplete
							? 'In Library'
							: libraryUnmatchedOnly
								? 'Unmatched files'
								: editionCounts
									? `${editionCounts.kept}/${editionCounts.total}`
									: `${libraryTrackCount}/${mbTrackCount}`}
					</span>
					{#if authStore.isAdmin}
						<Button
							variant="ghost"
							size="xs"
							icon={RefreshCw}
							loading={rescan.isPending}
							onclick={handleRescan}
						>
							Rescan
						</Button>
					{/if}
					{#if authStore.isTrusted && libraryBelowCutoff && downloadClientConfigured}
						<Button
							variant="ghost"
							size="xs"
							icon={TrendingUp}
							onclick={handleUpgrade}
							disabled={upgrade.isPending || upgradeQueued}
							title="Some tracks are below your quality cutoff - find a better copy"
						>
							{upgradeQueued ? 'Upgrade queued' : 'Upgrade quality'}
						</Button>
					{/if}
				</div>
			{/if}

			<div class="flex flex-wrap gap-x-4 gap-y-2 text-xs sm:text-sm opacity-70">
				{#if tracksInfo?.label}
					<div>
						<span class="font-semibold">Label:</span>
						{tracksInfo.label}
					</div>
				{/if}
				{#if tracksInfo?.country}
					<div>
						<span class="font-semibold">Country:</span>
						{tracksInfo.country}
					</div>
				{/if}
				{#if tracksInfo?.barcode}
					<div>
						<span class="font-semibold">Barcode:</span>
						{tracksInfo.barcode}
					</div>
				{/if}
			</div>

			{#if downloadClientConfigured}
				<div class="pt-4 flex flex-col gap-3">
					{#if headerDownloadTask}
						<AlbumDownloadStatus task={headerDownloadTask} {managementHeld} />
					{/if}
					<div class="flex flex-wrap items-start gap-3">
						{#if inLibrary || libraryInLibrary}
							{#if libraryUnmatchedOnly}
								<div class="badge badge-error h-8 gap-2 px-3">
									<TriangleAlert class="h-4 w-4" />
									Unmatched files only
								</div>
							{:else}
								<div class="badge h-8 gap-2 border-none bg-accent px-3 text-accent-content">
									<Check class="h-4 w-4" />
									{libraryComplete || !editionCounts
										? 'In Library'
										: `In Library • ${editionCounts.kept}/${editionCounts.total}`}
								</div>
							{/if}
							<AlbumDownloadButton
								albumId={localCopies[0]?.id || null}
								mbid={localCopies[0]?.id ? null : releaseGroupMbid || album.musicbrainz_id || null}
								totalSizeBytes={localCopies[0]?.total_size_bytes ?? null}
								trackCount={libraryTrackCount}
								{downloadAllowed}
								size="sm"
							/>
							{#if authStore.isAdmin}
								<Button variant="danger-ghost" size="sm" icon={Trash2} onclick={ondelete}>
									Remove
								</Button>
							{/if}
						{:else if isRequested}
							{#if !headerDownloadTask}
								<div class="badge badge-lg badge-warning gap-2">
									<Clock class="h-4 w-4" />
									Requested
								</div>
							{/if}
							{#if authStore.isAdmin}
								<Button variant="danger-ghost" size="sm" icon={Trash2} onclick={ondelete}>
									Remove
								</Button>
							{/if}
						{:else}
							<Button
								variant="accent"
								size="lg"
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
								size="lg"
								active={headerSampling}
								onclick={toggleHeaderSample}
								title="Hear 30-second samples of this album before you grab it"
							>
								{#if headerSampling && deckSampler.status === 'loading'}
									<span class="loading loading-spinner loading-sm"></span>
								{:else if headerSampling}
									<Square class="h-4 w-4" fill="currentColor" />
								{:else}
									<Disc3 class="h-5 w-5" />
								{/if}
								{headerSampling ? 'Stop sample' : 'Sample'}
							</Button>
						{/if}
					</div>
				</div>
			{/if}
		</div>
	</div>
</div>

<style>
	.album-hero {
		--hero-glow-color: var(--brand-hero);
		border: 1px solid rgb(var(--brand-hero) / 0.06);
		animation: hero-glow 4s ease-in-out infinite;
	}
	.album-hero:hover {
		border-color: rgb(var(--brand-hero) / 0.15);
	}
	@media (prefers-reduced-motion: reduce) {
		.album-hero {
			animation: none;
		}
	}
</style>
