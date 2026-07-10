<script lang="ts">
	import type { AlbumBasicInfo, AlbumTracksInfo, DownloadTask } from '$lib/types';
	import { getApiUrl } from '$lib/api/api-utils';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import BackButton from '$lib/components/BackButton.svelte';
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
		Square,
		TrendingUp,
		TriangleAlert,
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
	import { deckSampler } from '$lib/stores/deckSampler.svelte';

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
		coverageExpected?: number;
		coverageCovered?: number;
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
		coverageExpected = 0,
		coverageCovered = 0,
		releaseGroupMbid = '',
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

<div
	class="album-hero group relative isolate -mx-2 -mt-4 transition-all duration-500 sm:-mx-4 sm:-mt-8 lg:-mx-8"
>
	<!-- The clip lives on this backdrop-only layer, NOT the card: overflow-hidden on
	     the card would trap the Edition dropdown menu inside it. -->
	<div class="absolute inset-0 -z-10 overflow-hidden" aria-hidden="true">
		{#if backdropUrl}
			{#key backdropUrl}
				<img src={backdropUrl} alt="" class="album-hero__img" loading="lazy" />
			{/key}
		{/if}
		<div class="album-hero__wash"></div>
	</div>

	<div class="relative z-10 px-4 pt-6 pb-10 sm:px-8 sm:pt-8 sm:pb-12 lg:px-12">
		{#if (inLibrary || isRequested) && downloadClientConfigured}
			<button
				class="btn btn-sm btn-ghost btn-circle absolute top-4 right-4 z-20"
				onclick={onrefresh}
				disabled={refreshing}
				title="Refresh album status"
			>
				<RefreshCw class="h-5 w-5 {refreshing ? 'animate-spin' : ''}" />
			</button>
		{/if}

		<div class="mb-6">
			<BackButton />
		</div>

		<div class="flex flex-col gap-6 lg:flex-row lg:gap-10">
			<div class="w-full flex-shrink-0 lg:w-64 xl:w-80 lg:self-end">
				<AlbumImage
					mbid={album.musicbrainz_id}
					customUrl={album.cover_url}
					remoteUrl={album.album_thumb_url ?? null}
					alt={album.title}
					size="hero"
					lazy={false}
					rounded="xl"
					className="w-full aspect-square shadow-2xl ring-1 ring-base-content/10"
				/>
			</div>

			<div class="flex min-w-0 flex-1 flex-col space-y-4 lg:justify-end">
				<div class="album-hero__eyebrow">
					<span>{album.type || 'Album'}</span>
				</div>

				<h1
					class="hero-title font-display text-3xl font-bold leading-[1.02] tracking-tight sm:text-4xl lg:text-5xl xl:text-6xl"
				>
					{album.title}
				</h1>

				{#if album.disambiguation}
					<p class="text-sm italic text-base-content/60">({album.disambiguation})</p>
				{/if}

				<div class="flex flex-wrap items-center gap-2 text-sm text-base-content/60">
					<button
						onclick={onartistclick}
						class="cursor-pointer font-semibold text-base-content/80 transition-colors hover:text-base-content hover:underline"
					>
						{album.artist_name}
					</button>

					{#if album.year}
						<span class="text-base-content/30">·</span>
						<span class="font-mono tabular-nums">{album.year}</span>
					{/if}

					{#if tracksInfo && tracksInfo.total_tracks > 0}
						<span class="text-base-content/30">·</span>
						<span class="font-mono tabular-nums"
							>{tracksInfo.total_tracks} {tracksInfo.total_tracks === 1 ? 'track' : 'tracks'}</span
						>
					{:else if loadingTracks}
						<span class="text-base-content/30">·</span>
						<span class="skeleton w-16 h-4 inline-block"></span>
					{/if}

					{#if tracksInfo?.total_length}
						<span class="text-base-content/30">·</span>
						<span class="font-mono tabular-nums"
							>{formatTotalDuration(tracksInfo.total_length)}</span
						>
					{/if}
				</div>

				{#if authStore.isTrusted && downloadClientConfigured && editions.length > 0}
					<div class="flex flex-wrap items-center gap-2">
						<div class="dropdown">
							<button
								type="button"
								class="btn btn-ghost btn-xs gap-1 rounded-full border border-base-content/10"
								tabindex="0"
							>
								{#if editionsQuery.data?.pinned_release_mbid}
									<Pin class="h-3 w-3 text-primary" />
								{/if}
								Edition: {currentEdition ? editionLabel(currentEdition) : 'automatic'}
								<ChevronDown class="h-3 w-3" />
							</button>
							<ul
								class="dropdown-content menu menu-sm z-50 mt-1 max-h-72 w-80 flex-nowrap overflow-y-auto rounded-2xl border border-base-content/10 bg-base-100 p-1 shadow-lg"
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
								class="btn btn-ghost btn-xs gap-1 rounded-full border border-base-content/10 {acquireQueued
									? 'text-success'
									: 'text-accent'}"
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
						<span
							class="badge badge-sm gap-1 rounded-full {libraryComplete
								? 'border-accent/40 bg-accent/15 text-accent'
								: libraryUnmatchedOnly
									? 'badge-error badge-outline'
									: 'badge-warning badge-outline'}"
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
								class="btn btn-ghost btn-xs gap-1 rounded-full border border-base-content/10"
								onclick={handleRescan}
								disabled={rescan.isPending}
							>
								<RefreshCw class="h-3.5 w-3.5 {rescan.isPending ? 'animate-spin' : ''}" />
								Rescan
							</button>
							<button
								class="btn btn-ghost btn-xs gap-1 rounded-full border border-base-content/10"
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
								class="btn btn-ghost btn-xs gap-1 rounded-full border border-base-content/10 text-accent"
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

				<div
					class="flex flex-wrap gap-x-5 gap-y-1.5 font-mono text-[0.65rem] uppercase tracking-[0.14em] text-base-content/50"
				>
					{#if tracksInfo?.label}
						<div>
							<span class="font-bold text-base-content/35">Label</span>
							{tracksInfo.label}
						</div>
					{/if}
					{#if tracksInfo?.country}
						<div>
							<span class="font-bold text-base-content/35">Country</span>
							{tracksInfo.country}
						</div>
					{/if}
					{#if tracksInfo?.barcode}
						<div class="tabular-nums">
							<span class="font-bold text-base-content/35">Barcode</span>
							{tracksInfo.barcode}
						</div>
					{/if}
				</div>

				{#if downloadClientConfigured}
					<div class="pt-4 flex flex-col gap-3">
						{#if headerDownloadTask}
							<AlbumDownloadStatus task={headerDownloadTask} />
						{/if}
						<div class="flex flex-wrap items-center gap-3">
							{#if inLibrary || libraryInLibrary}
								{#if libraryUnmatchedOnly}
									<div
										class="inline-flex items-center gap-2 rounded-full border border-error/40 bg-error/10 px-4 py-2 text-sm font-semibold text-error"
									>
										<TriangleAlert class="h-4 w-4" />
										Unmatched files only
									</div>
								{:else}
									<div
										class="inline-flex items-center gap-2 rounded-full border border-accent/40 bg-accent/10 px-4 py-2 text-sm font-semibold text-accent"
									>
										<Check class="h-4 w-4" />
										{libraryComplete || !coverageKnown
											? 'In Library'
											: `In Library • ${coverageCovered}/${coverageExpected}`}
									</div>
								{/if}
								{#if authStore.isAdmin}
									<button
										class="btn btn-sm btn-error btn-outline gap-1 rounded-full"
										onclick={ondelete}
									>
										<Trash2 class="h-4 w-4" />
										Remove
									</button>
								{/if}
							{:else if isRequested}
								{#if !headerDownloadTask}
									<div
										class="inline-flex items-center gap-2 rounded-full border border-warning/40 bg-warning/10 px-4 py-2 text-sm font-semibold text-warning"
									>
										<Clock class="h-4 w-4" />
										Requested
									</div>
								{/if}
								{#if authStore.isAdmin}
									<button
										class="btn btn-sm btn-error btn-outline gap-1 rounded-full"
										onclick={ondelete}
									>
										<Trash2 class="h-4 w-4" />
										Remove
									</button>
								{/if}
							{:else}
								<button
									class="btn btn-primary gap-2 rounded-full"
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
									class="btn btn-ghost gap-2 rounded-full border border-base-content/15 bg-base-100/40"
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
</div>

<style>
	.album-hero__img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		object-position: center 30%;
		transform: scale(1.15);
		filter: saturate(0.9) brightness(0.62) blur(22px);
		transition: opacity var(--dn-dur-slow) ease;
	}

	/* charcoal wash — same technique as .dn-spotlight__wash on Home */
	.album-hero__wash {
		position: absolute;
		inset: 0;
		background:
			linear-gradient(
				90deg,
				oklch(from var(--color-base-100) l c h / 0.92) 0%,
				oklch(from var(--color-base-100) l c h / 0.55) 45%,
				oklch(from var(--color-base-100) l c h / 0.2) 100%
			),
			linear-gradient(
				180deg,
				oklch(from var(--color-base-100) l c h / 0.35) 0%,
				oklch(from var(--color-base-100) l c h / 0.1) 40%,
				var(--color-base-100) 100%
			);
	}

	.album-hero__eyebrow span {
		display: inline-block;
		padding: 0.32rem 0.85rem;
		border-radius: 999px;
		border: 1px solid var(--dn-hairline-strong);
		background: oklch(from var(--color-base-100) l c h / 0.45);
		backdrop-filter: blur(8px);
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.22em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.75);
	}
</style>
