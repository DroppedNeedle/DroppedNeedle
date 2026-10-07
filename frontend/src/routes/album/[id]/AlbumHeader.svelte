<script lang="ts">
	import { coverSrc } from '$lib/api/covers';
	import type {
		AlbumBasicInfo,
		AlbumTracksInfo,
		DownloadTask,
		HeldImport,
		LibraryAlbumSummary
	} from '$lib/types';
	import { colors } from '$lib/colors';
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
		TriangleAlert,
		ChevronDown,
		Hand
	} from 'lucide-svelte';
	import { rescanAlbum } from '$lib/queries/library/LibraryMutations.svelte';
	import { requestUpgradeAlbum } from '$lib/queries/downloads/UpgradeQueries.svelte';
	import { acquireEdition, getAlbumEditionsQuery } from '$lib/queries/albums/EditionQueries.svelte';
	import type { AlbumEditionItem } from '$lib/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { deckSampler } from '$lib/stores/deckSampler.svelte';
	import LocalAlbumIdentificationControl from './LocalAlbumIdentificationControl.svelte';
	import EditionPicker from '$lib/components/library/EditionPicker.svelte';
	import AlbumDownloadButton from './AlbumDownloadButton.svelte';

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
		coverageExpected?: number;
		coverageCovered?: number;
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
		coverageExpected = 0,
		coverageCovered = 0,
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
	// Coverage-aware library state (P5, 2026-07-05 incident): with a known tracklist,
	// "In Library" means COVERED - a wrong file squatting under the album reads
	// "Unmatched files", never owned. coverageExpected === 0 (tracklist unavailable)
	// falls back to the pre-P5 presence counting.
	const coverageKnown = $derived(coverageExpected > 0);
	const libraryComplete = $derived(
		coverageKnown
			? libraryInLibrary && coverageCovered >= coverageExpected
			: libraryInLibrary && mbTrackCount > 0 && libraryTrackCount >= mbTrackCount
	);
	const libraryUnmatchedOnly = $derived(
		coverageKnown && libraryInLibrary && libraryTrackCount > 0 && coverageCovered === 0
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

	// Album quality upgrade (admin/trusted, CollectionManagement D18): fetch a
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

	// Edition selection (admin/trusted, CollectionManagement Feature E): pick the
	// MB release the album page + acquisition follow (D16), and acquire it (D13).
	// ST7 W2: the query warms as soon as the album identity is known - the old
	// !loadingTracks term serialized a depth-3 chain for no data dependency
	// (editions key off the authenticated user and RG mbid). The picker section
	// below still waits on !loadingTracks so the visible behavior is unchanged.
	const editionsMbid = $derived(releaseGroupMbid || album.musicbrainz_id);
	const editionsQuery = getAlbumEditionsQuery(
		() => authStore.user?.id,
		() => editionsMbid,
		() => authStore.isTrusted && downloadClientConfigured && Boolean(editionsMbid)
	);
	const editions = $derived(editionsQuery.data?.items ?? []);
	const pinnedEdition = $derived(editions.find((edition) => edition.is_pinned) ?? null);
	const currentEdition = $derived(
		pinnedEdition ??
			editions.find(
				(edition) =>
					edition.release_mbid ===
					(tracksInfo?.selected_release_mbid ?? editionsQuery.data?.selected_release_mbid)
			) ??
			null
	);
	const hasEffectivePin = $derived(pinnedEdition !== null);
	const editionActionLabel = $derived(
		!libraryInLibrary
			? 'Acquire this edition'
			: libraryComplete
				? 'Upgrade this edition'
				: 'Complete this edition'
	);
	const editionActionTitle = $derived(
		!libraryInLibrary
			? "Request this edition's tracks"
			: libraryComplete
				? "Upgrade this edition's below-cutoff tracks"
				: "Request this edition's missing tracks and upgrade its below-cutoff ones"
	);
	const acquireMutation = acquireEdition();

	// SvelteKit reuses this component instance across album navigations, so the
	// per-album "queued" button states must reset when the album changes
	$effect(() => {
		void editionsMbid;
		upgradeQueued = false;
		acquireQueued = false;
	});

	function editionLabel(e: AlbumEditionItem): string {
		const bits = [
			e.disambiguation,
			e.date?.slice(0, 4),
			e.country,
			`${e.track_count} tracks`
		].filter(Boolean);
		return bits.join(' · ') || e.release_mbid.slice(0, 8);
	}

	// One picker for every way of choosing an edition. The choice lands on a
	// library copy; with several copies the picker asks which one.
	let picker = $state<{ open: () => void } | null>(null);
	const pickerCopies = $derived(
		localCopies.flatMap((copy) => (copy.id ? [{ id: copy.id, title: copy.title }] : []))
	);

	// after a successful acquire the button parks as "Queued" (server-side dedup
	// makes a re-click harmless, but the UI shouldn't invite one)
	let acquireQueued = $state(false);
	async function handleAcquireEdition() {
		try {
			const result = await acquireMutation.mutateAsync({ mbid: editionsMbid });
			if (result.status === 'already_complete') {
				toastStore.show({
					message: 'Nothing to do - this edition is complete and at your cutoff.',
					type: 'info'
				});
			} else {
				acquireQueued = true;
				toastStore.show({ message: result.message, type: 'success' });
			}
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Could not acquire this edition',
				type: 'error'
			});
		}
	}

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
			<button
				class="absolute top-3 right-3 btn btn-sm btn-ghost btn-circle z-20"
				onclick={onrefresh}
				disabled={refreshing}
				title="Refresh album status"
			>
				<RefreshCw class="h-5 w-5 {refreshing ? 'animate-spin' : ''}" />
			</button>
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
				<button onclick={onartistclick} class="font-semibold hover:underline cursor-pointer">
					{album.artist_name}
				</button>

				{#if album.year}
					<span class="opacity-50">•</span>
					<span>{album.year}</span>
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

			{#if authStore.isTrusted && downloadClientConfigured && !loadingTracks && editions.length > 0}
				<div class="flex flex-wrap items-center gap-2">
					<span class="inline-flex items-center gap-1 text-xs text-base-content/70">
						{#if hasEffectivePin}
							<Hand class="h-3 w-3 text-primary" aria-label="Chosen by a person" />
						{/if}
						Edition: {currentEdition
							? `${hasEffectivePin ? '' : 'Automatic · '}${editionLabel(currentEdition)}`
							: 'Automatic'}
					</span>
					{#if pickerCopies.length}
						<button type="button" class="btn btn-ghost btn-xs" onclick={() => picker?.open()}
							>Change edition</button
						>
					{/if}
					{#if currentEdition}
						{#if libraryComplete && !libraryBelowCutoff}
							<span
								class="inline-flex h-6 items-center gap-1 px-2 text-xs font-medium text-success"
								title="This edition is complete and meets your quality cutoff"
							>
								<Check class="h-3.5 w-3.5" />
								Edition complete
							</span>
						{:else}
							<button
								class="btn btn-ghost btn-xs gap-1 {acquireQueued ? 'text-success' : 'text-primary'}"
								onclick={handleAcquireEdition}
								disabled={acquireMutation.isPending || acquireQueued}
								title={editionActionTitle}
							>
								{#if acquireMutation.isPending}
									<span class="loading loading-spinner loading-xs"></span>
									Acquiring...
								{:else if acquireQueued}
									<Check class="h-3.5 w-3.5" />
									Acquisition queued
								{:else}
									<Plus class="h-3.5 w-3.5" />
									{editionActionLabel}
								{/if}
							</button>
						{/if}
					{/if}
				</div>
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
								: coverageKnown
									? `${coverageCovered}/${coverageExpected}`
									: `${libraryTrackCount}/${mbTrackCount}`}
					</span>
					{#if authStore.isAdmin}
						<button
							class="btn btn-ghost btn-xs gap-1"
							onclick={handleRescan}
							disabled={rescan.isPending}
						>
							<RefreshCw class="h-3.5 w-3.5 {rescan.isPending ? 'animate-spin' : ''}" />
							Rescan
						</button>
						{#if localCopies.length === 1}
							<LocalAlbumIdentificationControl album={localCopies[0]} />
						{:else if localCopies.length > 1}
							<details class="dropdown dropdown-end">
								<summary class="btn btn-ghost btn-xs gap-1">
									<RefreshCw class="h-3.5 w-3.5" /> Re-identify copy...
									<ChevronDown class="h-3.5 w-3.5" />
								</summary>
								<div
									class="dropdown-content z-20 mt-2 w-72 rounded-box border border-base-content/10 bg-base-100 p-2 shadow-xl"
								>
									<p class="px-2 py-1 text-xs font-semibold text-base-content/55">
										Choose a local copy
									</p>
									{#each localCopies as localCopy (localCopy.id)}
										<div class="flex items-center gap-2 rounded-lg px-2 py-2 hover:bg-base-200">
											<div class="min-w-0 flex-1">
												<p class="truncate text-sm font-medium">{localCopy.title}</p>
												<p class="truncate text-xs text-base-content/55">
													{localCopy.artist_name} · {localCopy.track_count} tracks
												</p>
											</div>
											<LocalAlbumIdentificationControl album={localCopy} />
										</div>
									{/each}
								</div>
							</details>
						{/if}
					{/if}
					{#if authStore.isTrusted && libraryBelowCutoff && downloadClientConfigured}
						<button
							class="btn btn-ghost btn-xs gap-1 text-primary"
							onclick={handleUpgrade}
							disabled={upgrade.isPending || upgradeQueued}
							title="Some tracks are below your quality cutoff - find a better copy"
						>
							<TrendingUp class="h-3.5 w-3.5" />
							{upgradeQueued ? 'Upgrade queued' : 'Upgrade quality'}
						</button>
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
								<div
									class="badge h-8 gap-2 px-3"
									style="background-color: {colors.accent}; color: {colors.secondary};"
								>
									<Check class="h-4 w-4" />
									{libraryComplete || !coverageKnown
										? 'In Library'
										: `In Library • ${coverageCovered}/${coverageExpected}`}
								</div>
							{/if}
							<AlbumDownloadButton
								albumId={localCopies[0]?.id || null}
								mbid={localCopies[0]?.id ? null : releaseGroupMbid || album.musicbrainz_id || null}
								totalSizeBytes={localCopies[0]?.total_size_bytes ?? null}
								trackCount={libraryTrackCount}
								{downloadAllowed}
								className="btn-sm"
							/>
							{#if authStore.isAdmin}
								<button class="btn btn-sm btn-error btn-outline gap-1" onclick={ondelete}>
									<Trash2 class="h-4 w-4" />
									Remove
								</button>
							{/if}
						{:else if isRequested}
							{#if !headerDownloadTask}
								<div class="badge badge-lg badge-warning gap-2">
									<Clock class="h-4 w-4" />
									Requested
								</div>
							{/if}
							{#if authStore.isAdmin}
								<button class="btn btn-sm btn-error btn-outline gap-1" onclick={ondelete}>
									<Trash2 class="h-4 w-4" />
									Remove
								</button>
							{/if}
						{:else}
							<button
								class="btn btn-lg gap-2"
								style="background-color: {colors.accent}; color: {colors.secondary}; border: none;"
								onclick={() => onrequest()}
								disabled={requesting}
							>
								{#if requesting}
									<span class="loading loading-spinner loading-sm"></span>
									Requesting...
								{:else}
									<Plus class="h-5 w-5" />
									Add to Library
								{/if}
							</button>
						{/if}
						{#if !inLibrary}
							<button
								class="btn btn-lg btn-ghost gap-2 border border-base-content/15"
								class:btn-active={headerSampling}
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
							</button>
						{/if}
					</div>
				</div>
			{/if}
		</div>
	</div>
</div>
{#if pickerCopies.length}
	<EditionPicker
		bind:this={picker}
		groupMbid={editionsMbid}
		copies={pickerCopies}
		onchanged={onrefresh}
	/>
{/if}

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
