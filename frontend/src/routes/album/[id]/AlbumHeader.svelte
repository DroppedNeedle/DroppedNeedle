<script lang="ts">
	import type { AlbumBasicInfo, AlbumTracksInfo, DownloadTask } from '$lib/types';
	import { getApiUrl } from '$lib/api/api-utils';
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
		ScanSearch,
		Disc3,
		TrendingUp,
		ChevronDown,
		Pin
	} from 'lucide-svelte';
	import { rescanAlbum, reidentifyAlbum } from '$lib/queries/library/LibraryMutations.svelte';
	import { requestUpgradeAlbum } from '$lib/queries/downloads/UpgradeQueries.svelte';
	import {
		acquireEdition,
		clearEditionPin,
		getAlbumEditionsQuery,
		setEditionPin
	} from '$lib/queries/albums/EditionQueries.svelte';
	import type { AlbumEditionItem } from '$lib/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';

	interface Props {
		album: AlbumBasicInfo;
		tracksInfo: AlbumTracksInfo | null;
		loadingTracks: boolean;
		inLibrary: boolean;
		isRequested: boolean;
		requesting: boolean;
		refreshing: boolean;
		headerDownloadTask: DownloadTask | null;
		downloadClientConfigured: boolean;

		libraryInLibrary?: boolean;
		libraryTrackCount?: number;
		mbTrackCount?: number;
		libraryBelowCutoff?: boolean;
		releaseGroupMbid?: string;
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
		downloadClientConfigured,
		libraryInLibrary = false,
		libraryTrackCount = 0,
		mbTrackCount = 0,
		libraryBelowCutoff = false,
		releaseGroupMbid = '',
		onrequest,
		ondelete,
		onrefresh,
		onartistclick
	}: Props = $props();

	const rescan = rescanAlbum();
	const libraryComplete = $derived(
		libraryInLibrary && mbTrackCount > 0 && libraryTrackCount >= mbTrackCount
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
	const editionsMbid = $derived(releaseGroupMbid || album.musicbrainz_id);
	const editionsQuery = getAlbumEditionsQuery(
		() => editionsMbid,
		() => authStore.isTrusted && downloadClientConfigured
	);
	const editions = $derived(editionsQuery.data?.items ?? []);
	const currentEdition = $derived(
		editions.find((e) => e.is_pinned) ?? editions.find((e) => e.is_owned) ?? null
	);
	const pinMutation = setEditionPin();
	const clearPinMutation = clearEditionPin();
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

	async function handlePickEdition(releaseMbid: string | null) {
		// the DaisyUI dropdown is focus-driven: blur the trigger so the menu
		// closes on selection instead of hanging over the refreshed page
		(document.activeElement as HTMLElement | null)?.blur();
		try {
			if (releaseMbid === null) {
				await clearPinMutation.mutateAsync({ mbid: editionsMbid });
				toastStore.show({ message: 'Edition back to automatic.', type: 'success' });
			} else {
				await pinMutation.mutateAsync({ mbid: editionsMbid, releaseMbid });
				toastStore.show({ message: 'Edition pinned.', type: 'success' });
			}
			onrefresh(); // the pin changes the served tracklist - refetch the page
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Could not change the edition',
				type: 'error'
			});
		}
	}

	// after a successful acquire the button parks as "Queued" (server-side dedup
	// makes a re-click harmless, but the UI shouldn't invite one)
	let acquireQueued = $state(false);
	async function handleAcquireEdition() {
		try {
			const result = await acquireMutation.mutateAsync({ mbid: editionsMbid });
			if (result.requested === 0 && result.upgrades === 0) {
				toastStore.show({
					message: 'Nothing to do - this edition is complete and at your cutoff.',
					type: 'info'
				});
			} else {
				acquireQueued = true;
				toastStore.show({
					message: `Requested ${result.requested} missing and ${result.upgrades} upgrade${
						result.upgrades === 1 ? '' : 's'
					} for this edition.`,
					type: 'success'
				});
			}
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Could not acquire this edition',
				type: 'error'
			});
		}
	}

	const reidentify = reidentifyAlbum();
	// Re-decide which album these files are (correction path), vs Rescan which only
	// refreshes their tags. Non-destructive - it re-attributes, never deletes.
	async function handleReidentify() {
		try {
			await reidentify.mutateAsync(releaseGroupMbid);
			toastStore.show({ message: 'Re-identify started.', type: 'success' });
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Re-identify failed',
				type: 'error'
			});
		}
	}

	let backdropUrl = $derived(
		album.cover_url ||
			album.album_thumb_url ||
			(album.musicbrainz_id
				? getApiUrl(`/api/v1/covers/release-group/${album.musicbrainz_id}?size=250`)
				: null)
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

			{#if authStore.isTrusted && downloadClientConfigured && editions.length > 0}
				<div class="flex flex-wrap items-center gap-2">
					<div class="dropdown">
						<button type="button" class="btn btn-ghost btn-xs gap-1" tabindex="0">
							{#if editionsQuery.data?.pinned_release_mbid}
								<Pin class="h-3 w-3 text-primary" />
							{/if}
							Edition: {currentEdition ? editionLabel(currentEdition) : 'automatic'}
							<ChevronDown class="h-3 w-3" />
						</button>
						<ul
							class="dropdown-content menu menu-sm z-50 mt-1 max-h-72 w-80 flex-nowrap overflow-y-auto rounded-box border border-base-300 bg-base-100 p-1 shadow-lg"
						>
							<li>
								<button
									type="button"
									class:font-semibold={!editionsQuery.data?.pinned_release_mbid}
									onclick={() => void handlePickEdition(null)}
								>
									Automatic (follow the owned edition)
								</button>
							</li>
							{#each editions as edition (edition.release_mbid)}
								<li>
									<button
										type="button"
										class="justify-between gap-2"
										class:font-semibold={edition.is_pinned}
										onclick={() => void handlePickEdition(edition.release_mbid)}
									>
										<span class="truncate">{editionLabel(edition)}</span>
										<span class="flex shrink-0 gap-1">
											{#if edition.is_owned}
												<span class="badge badge-success badge-xs">owned</span>
											{/if}
											{#if edition.is_pinned}
												<span class="badge badge-primary badge-xs">pinned</span>
											{/if}
										</span>
									</button>
								</li>
							{/each}
						</ul>
					</div>
					{#if currentEdition}
						<button
							class="btn btn-ghost btn-xs gap-1 {acquireQueued ? 'text-success' : 'text-primary'}"
							onclick={handleAcquireEdition}
							disabled={acquireMutation.isPending || acquireQueued}
							title="Request this edition's missing tracks and upgrade its below-cutoff ones"
						>
							{#if acquireMutation.isPending}
								<span class="loading loading-spinner loading-xs"></span>
								Acquiring...
							{:else if acquireQueued}
								<Check class="h-3.5 w-3.5" />
								Acquisition queued
							{:else}
								<Plus class="h-3.5 w-3.5" />
								Acquire this edition
							{/if}
						</button>
					{/if}
				</div>
			{/if}

			{#if libraryInLibrary}
				<div class="flex flex-wrap items-center gap-2">
					<span class="badge badge-sm gap-1 {libraryComplete ? 'badge-success' : 'badge-warning'}">
						<Disc3 class="h-3.5 w-3.5" />
						{libraryComplete ? 'In Library' : `${libraryTrackCount}/${mbTrackCount}`}
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
						<button
							class="btn btn-ghost btn-xs gap-1"
							onclick={handleReidentify}
							disabled={reidentify.isPending}
							title="Re-match this album's files from scratch (fixes a wrong release)"
						>
							<ScanSearch class="h-3.5 w-3.5 {reidentify.isPending ? 'animate-spin' : ''}" />
							Re-identify
						</button>
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
						<AlbumDownloadStatus task={headerDownloadTask} />
					{/if}
					<div class="flex flex-wrap items-start gap-3">
						{#if inLibrary || libraryInLibrary}
							<div
								class="badge badge-lg gap-2"
								style="background-color: {colors.accent}; color: {colors.secondary};"
							>
								<Check class="h-4 w-4" />
								In Library
							</div>
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
