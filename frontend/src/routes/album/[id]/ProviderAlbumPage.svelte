<script lang="ts">
	import { goto } from '$app/navigation';
	import { integrationStore } from '$lib/stores/integration';
	import BackButton from '$lib/components/BackButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import LastFmAlbumEnrichmentComponent from '$lib/components/LastFmAlbumEnrichment.svelte';
	import DeleteAlbumModal from '$lib/components/DeleteAlbumModal.svelte';
	import RemoveTrackFileDialog from './RemoveTrackFileDialog.svelte';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import { ApiError } from '$lib/api/client';
	import { createAlbumPageState } from './albumPageState.svelte';
	import AlbumHeader from './AlbumHeader.svelte';
	import UnmatchedFilesSection from './UnmatchedFilesSection.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AlbumSourceBars from './AlbumSourceBars.svelte';
	import AlbumTrackList from './AlbumTrackList.svelte';
	import AlbumDiscovery from './AlbumDiscovery.svelte';
	import WhereToBuy from './WhereToBuy.svelte';
	import AlbumPluginDetails from './AlbumPluginDetails.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import LibraryAlbumCard from '$lib/components/library/LibraryAlbumCard.svelte';
	import ManagementHoldSection from '$lib/components/library/ManagementHoldSection.svelte';
	import RecycledRemovals from '$lib/components/library/RecycledRemovals.svelte';
	import AlbumHistory from '$lib/components/library/AlbumHistory.svelte';
	import RetagPairCard from '$lib/components/library/RetagPairCard.svelte';
	import { getRetagPairsQuery } from '$lib/queries/library/LibraryRetagPairQueries.svelte';
	import {
		getLibraryAlbumCopiesQuery,
		getLibraryAlbumDetailQuery,
		getLibraryAlbumStatusQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import LocalAlbumPage from './LocalAlbumPage.svelte';
	import { getRatingsQuery, ratingOf, setRating } from '$lib/queries/ratings/RatingQueries.svelte';

	// The outage fallback only needs the local id; the v3 album view the
	// route gate passes satisfies this, as does the v1 detail shape.
	interface Props {
		data: { albumId: string };
		localAlbum?: { id: string };
	}

	let { data, localAlbum }: Props = $props();

	const state = createAlbumPageState(() => data.albumId);
	// The page is keyed by release group, but the library reads take local
	// album ids: the album status names the local album holding the group,
	// and its copies are the group's other local albums. A status that echoes
	// the asked id back found no local album.
	const libraryStatusQuery = getLibraryAlbumStatusQuery(() => data.albumId);
	const localAlbumId = $derived.by(() => {
		if (localAlbum) return localAlbum.id;
		const statusAlbumId = libraryStatusQuery.data?.album_id;
		return statusAlbumId && statusAlbumId !== data.albumId ? statusAlbumId : '';
	});
	const localAlbumQuery = getLibraryAlbumDetailQuery(() => localAlbumId);
	const localCopiesQuery = getLibraryAlbumCopiesQuery(() => localAlbumId);
	const fallbackAlbum = $derived(localAlbum ?? localAlbumQuery.data ?? null);
	const localCopies = $derived(
		localAlbumQuery.data ? [localAlbumQuery.data, ...(localCopiesQuery.data?.items ?? [])] : []
	);
	// One ratings read for the local album and the files behind its rows,
	// once the album status is in. Only a library album is rated.
	const ratingsQuery = getRatingsQuery(() =>
		localAlbumId && libraryStatusQuery.isSuccess
			? {
					albumIds: [localAlbumId],
					trackIds: (libraryStatusQuery.data?.tracks ?? []).map((track) => track.id)
				}
			: null
	);
	const rate = setRating();
	const retagPairsQuery = getRetagPairsQuery(() => authStore.isAdmin);
	const retagPairs = $derived(
		localAlbumId
			? (retagPairsQuery.data?.pairs ?? []).filter(
					(pair) => pair.old_album.id === localAlbumId || pair.new_album.id === localAlbumId
				)
			: []
	);
	const primaryError = $derived(state.primaryError);
	const albumNotFound = $derived(primaryError instanceof ApiError && primaryError.status === 404);
	const providerUnavailable = $derived.by(() => {
		if (!(primaryError instanceof ApiError)) return false;
		return primaryError.status === 0 || primaryError.status === 429 || primaryError.status >= 500;
	});

	$effect(() => {
		const canonicalId = state.album?.musicbrainz_id;
		if (canonicalId && canonicalId !== data.albumId) {
			void goto(albumHref(canonicalId), { replaceState: true });
		}
	});
</script>

<div class="w-full px-2 sm:px-4 lg:px-8 py-4 sm:py-8 max-w-7xl mx-auto">
	<div class="mb-4">
		<BackButton />
	</div>

	{#if albumNotFound}
		<div class="flex items-center justify-center min-h-[50vh]">
			<div class="alert alert-error">
				<span>Album not found.</span>
			</div>
		</div>
	{:else if providerUnavailable && fallbackAlbum}
		<div class="mb-4 flex justify-center">
			<div class="alert alert-info text-sm">
				<span
					>MusicBrainz is unreachable right now, so this page is built from your local files. Some
					extras are hidden until it returns.</span
				>
			</div>
		</div>
		<LocalAlbumPage albumId={fallbackAlbum.id} />
	{:else if providerUnavailable}
		<div class="flex items-center justify-center min-h-[50vh]">
			<div class="alert alert-error">
				<span>MusicBrainz is temporarily unavailable.</span>
				<Button variant="ghost" size="sm" onclick={() => void state.refreshAll()}>Retry</Button>
			</div>
		</div>
	{:else if state.error || primaryError}
		<div class="flex items-center justify-center min-h-[50vh]">
			<div class="alert alert-error">
				<span>{state.error ?? 'Error loading album'}</span>
			</div>
		</div>
	{:else if state.loadingBasic || !state.album}
		<div class="space-y-6 sm:space-y-8">
			<div class="flex flex-col lg:flex-row gap-6 lg:gap-8">
				<div class="skeleton w-full lg:w-64 xl:w-80 aspect-square rounded-box shrink-0"></div>
				<div class="flex-1 flex flex-col justify-end space-y-4">
					<div class="skeleton h-4 w-20"></div>
					<div class="skeleton h-12 w-3/4"></div>
					<div class="skeleton h-6 w-1/2"></div>
					<div class="flex gap-4 mt-6">
						<div class="skeleton h-12 w-32"></div>
						<div class="skeleton h-12 w-32"></div>
					</div>
				</div>
			</div>

			<div class="space-y-2">
				<div class="skeleton h-8 w-32 mb-4"></div>
				{#each Array(8) as _, i (`skeleton-${i}`)}
					<div class="skeleton h-12 w-full"></div>
				{/each}
			</div>
		</div>
	{:else if state.album}
		{@const album = state.album}
		<div class="space-y-6 sm:space-y-8">
			<AlbumHeader
				{album}
				tracksInfo={state.tracksInfo}
				loadingTracks={state.loadingTracks}
				inLibrary={state.inLibrary}
				isRequested={state.isRequested}
				requesting={state.requesting}
				refreshing={state.refreshing}
				headerDownloadTask={state.headerDownloadTask}
				heldFiles={state.headerHeldFiles}
				downloadClientConfigured={$integrationStore.download_client}
				libraryInLibrary={state.libraryInLibrary}
				libraryTrackCount={state.libraryTrackCount}
				libraryBelowCutoff={state.libraryBelowCutoff}
				editionCounts={state.editionCounts}
				mbTrackCount={state.tracksInfo?.total_tracks ?? 0}
				releaseGroupMbid={album.musicbrainz_id}
				{localCopies}
				editionPick={state.editionPick}
				downloadAllowed={state.localMatch?.download_allowed !== false}
				rating={ratingOf(ratingsQuery.data, 'album', localAlbumId)}
				onrate={localAlbumId
					? (rating) => rate.mutate({ kind: 'album', itemId: localAlbumId, rating })
					: undefined}
				onrequest={state.handleRequest}
				ondelete={state.handleDeleteClick}
				onrefresh={state.refreshAll}
				onartistclick={state.goToArtist}
			/>

			{#if authStore.isAdmin && localAlbumQuery.data}
				<ManagementHoldSection album={localAlbumQuery.data} />
			{/if}

			{#if authStore.isTrusted && localAlbumId}
				<RecycledRemovals
					albumId={localAlbumId}
					releaseGroupMbid={album.musicbrainz_id}
					onputback={() => void state.refreshAll()}
				/>
			{/if}

			{#each retagPairs as pair (`${pair.old_album.id}/${pair.new_album.id}`)}
				<RetagPairCard {pair} currentAlbumId={localAlbumId} />
			{/each}

			{#if localCopies.length > 1}
				<Card variant="bordered" padding="md" as="section" aria-label="Copies in your library">
					<h2 class="text-lg font-bold">Copies in your library</h2>
					<p class="mt-1 max-w-2xl text-sm text-base-content/55">
						This MusicBrainz release matches more than one local album. Choose the copy you want to
						open.
					</p>
					<div class="mt-4 grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
						{#each localCopies as localCopy (localCopy.id)}
							<LibraryAlbumCard album={localCopy} />
						{/each}
					</div>
				</Card>
			{/if}

			<WhereToBuy releaseGroupMbid={album.musicbrainz_id} enabled={!state.loadingTracks} />

			{#if state.loadingTracks}
				<div class="space-y-3">
					<h2 class="text-xl sm:text-2xl font-bold">Tracks</h2>
					<div class="bg-base-200 rounded-box overflow-hidden">
						<ul class="list">
							{#each Array(8) as _, i (`track-skeleton-${i}`)}
								<li class="list-row p-3 sm:p-4">
									<div class="flex items-center gap-4 w-full">
										<div class="skeleton w-8 h-4"></div>
										<div class="skeleton flex-1 h-4"></div>
										<div class="skeleton w-12 h-4"></div>
									</div>
								</li>
							{/each}
						</ul>
					</div>
				</div>
			{:else if state.tracksInfo && state.tracksInfo.tracks.length > 0}
				<div class="space-y-3">
					<div class="flex items-center justify-between flex-wrap gap-2">
						<h2 class="text-xl sm:text-2xl font-bold">Tracks</h2>
						{#if state.quota}
							<div class="flex items-center gap-2">
								<progress
									class="progress progress-accent w-20 h-1.5"
									value={state.quota.used}
									max={state.quota.limit}
								></progress>
								<span class="text-xs opacity-60">{state.quota.remaining}/{state.quota.limit}</span>
							</div>
						{/if}
					</div>

					<AlbumSourceBars
						{album}
						tracksInfo={state.tracksInfo}
						trackLinks={state.trackLinks}
						albumLink={state.albumLink}
						jellyfinMatch={state.jellyfinMatch}
						localMatch={state.localMatchForPlayback}
						navidromeMatch={state.navidromeMatch}
						plexMatch={state.plexMatch}
						loadingJellyfin={state.loadingJellyfin}
						loadingLocal={state.loadingLocal}
						loadingNavidrome={state.loadingNavidrome}
						loadingPlex={state.loadingPlex}
						youtubeEnabled={$integrationStore.youtube}
						youtubeApiConfigured={$integrationStore.youtube_api}
						jellyfinEnabled={$integrationStore.jellyfin}
						localfilesEnabled={$integrationStore.localfiles}
						navidromeEnabled={$integrationStore.navidrome}
						plexEnabled={$integrationStore.plex}
						jellyfinCallbacks={state.jellyfinCallbacks}
						localCallbacks={state.localCallbacks}
						localDownloadCallback={state.localDownloadCallback}
						navidromeCallbacks={state.navidromeCallbacks}
						plexCallbacks={state.plexCallbacks}
						onTrackLinksUpdate={state.handleTrackLinksUpdate}
						onAlbumLinkUpdate={state.handleAlbumLinkUpdate}
						onQuotaUpdate={state.handleQuotaUpdate}
					/>

					<AlbumTrackList
						{album}
						renderedTrackSections={state.renderedTrackSections}
						trackLinkMap={state.trackLinkMap}
						jellyfinMatch={state.jellyfinMatch}
						localMatch={state.localMatch}
						navidromeMatch={state.navidromeMatch}
						plexMatch={state.plexMatch}
						jellyfinTrackMap={state.jellyfinTrackMap}
						localTrackMap={state.localTrackMap}
						navidromeTrackMap={state.navidromeTrackMap}
						plexTrackMap={state.plexTrackMap}
						jellyfinTracks={state.jellyfinTracks}
						localTracks={state.localTracks}
						navidromeTracks={state.navidromeTracks}
						plexTracks={state.plexTracks}
						trackLinks={state.trackLinks}
						youtubeEnabled={$integrationStore.youtube}
						youtubeApiConfigured={$integrationStore.youtube_api}
						jellyfinEnabled={$integrationStore.jellyfin}
						localfilesEnabled={$integrationStore.localfiles}
						navidromeEnabled={$integrationStore.navidrome}
						plexEnabled={$integrationStore.plex}
						libraryTracksByRecording={state.libraryTracksByRecording}
						libraryTracksByPosition={state.libraryTracksByPosition}
						heldByRecording={state.heldByRecording}
						heldByPosition={state.heldByPosition}
						trackDownloadTasks={state.trackDownloadTasks}
						editionRows={state.editionRows}
						releaseGroupMbid={album.musicbrainz_id}
						releaseMbid={state.tracksInfo?.selected_release_mbid}
						trackRatings={ratingsQuery.data?.tracks}
						onRateTrack={(trackId, rating) =>
							rate.mutate({ kind: 'track', itemId: trackId, rating })}
						onPlaySourceTrack={state.playSourceTrack}
						onTrackGenerated={state.handleTrackGenerated}
						onQuotaUpdate={state.handleQuotaUpdate}
						getTrackContextMenuItems={state.getTrackContextMenuItems}
					/>

					<UnmatchedFilesSection
						files={state.libraryUnmatchedFiles}
						albumMbid={album.musicbrainz_id}
						albumId={localAlbumId}
						canRemove={authStore.isTrusted}
					/>

					{#if localAlbumId}
						<AlbumHistory albumId={localAlbumId} />
					{/if}

					<AddToPlaylistModal bind:this={state.playlistModalRef} />
				</div>
			{:else if state.tracksError}
				<div class="space-y-3">
					<h2 class="text-xl sm:text-2xl font-bold">Tracks</h2>
					<div class="alert alert-warning">
						<span>Couldn't load the track list.</span>
						<Button variant="ghost" size="sm" onclick={state.retryTracks}>Retry</Button>
					</div>
				</div>
			{:else}
				<div class="space-y-3">
					<h2 class="text-xl sm:text-2xl font-bold">Tracks</h2>
					<div class="alert alert-warning">
						<span>No tracks available.</span>
						<Button variant="ghost" size="sm" onclick={state.retryTracks}>Retry</Button>
					</div>
				</div>
			{/if}

			{#if album.release_date}
				<div class="text-xs opacity-60">
					<span class="font-semibold">Release Date:</span>
					{album.release_date}
				</div>
			{/if}

			{#if state.loadingLastfm || state.lastfmEnrichment}
				<LastFmAlbumEnrichmentComponent
					enrichment={state.lastfmEnrichment}
					loading={state.loadingLastfm}
				/>
			{/if}

			<AlbumPluginDetails
				releaseGroupMbid={album.musicbrainz_id}
				hasNotes={!!state.lastfmEnrichment?.summary}
				knownTags={(state.lastfmEnrichment?.tags ?? []).map((tag) => tag.name)}
			/>

			<AlbumDiscovery
				moreByArtist={state.moreByArtist}
				similarAlbums={state.similarAlbums}
				loadingDiscovery={state.loadingDiscovery}
				artistName={album.artist_name}
			/>
		</div>
	{:else}
		<div class="flex items-center justify-center min-h-[50vh]">
			<p class="text-base-content/60">Album not found</p>
		</div>
	{/if}
</div>

{#if state.removeFileTarget}
	<RemoveTrackFileDialog
		trackTitle={state.removeFileTarget.title}
		removing={state.removeFilePending}
		error={state.removeFileError}
		onconfirm={state.confirmRemoveFile}
		onclose={state.closeRemoveFileDialog}
	/>
{/if}

{#if state.showDeleteModal && state.album}
	<DeleteAlbumModal
		albumTitle={state.album.title}
		artistName={state.album.artist_name}
		musicbrainzId={state.album.musicbrainz_id}
		ondeleted={state.handleDeleted}
		onclose={() => {
			state.showDeleteModal = false;
		}}
	/>
{/if}
