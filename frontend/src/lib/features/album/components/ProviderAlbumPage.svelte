<script lang="ts">
	import { goto } from '$app/navigation';
	import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
	import BackButton from '$lib/ui/BackButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import LastFmAlbumEnrichmentComponent from '$lib/features/lastfm/components/LastFmAlbumEnrichment.svelte';
	import DeleteAlbumModal from '$lib/features/album/components/DeleteAlbumModal.svelte';
	import RemoveTrackFileDialog from './RemoveTrackFileDialog.svelte';
	import AddToPlaylistModal from '$lib/features/playlists/components/AddToPlaylistModal.svelte';
	import { ApiError } from '$lib/api/errors';
	import { createAlbumPageState } from '../state/albumPageState.svelte';
	import AlbumHeader from './AlbumHeader.svelte';
	import AlbumHeroSkeleton from './AlbumHeroSkeleton.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';
	import UnmatchedFilesSection from './UnmatchedFilesSection.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AlbumSourceBars from './AlbumSourceBars.svelte';
	import AlbumTrackList from './AlbumTrackList.svelte';
	import AlbumDiscovery from './AlbumDiscovery.svelte';
	import WhereToBuy from './WhereToBuy.svelte';
	import AlbumPluginDetails from './AlbumPluginDetails.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import LibraryAlbumCard from '$lib/features/library/components/LibraryAlbumCard.svelte';
	import ManagementHoldSection from '$lib/features/library-management/components/ManagementHoldSection.svelte';
	import RecycledRemovals from '$lib/features/library-management/components/RecycledRemovals.svelte';
	import AlbumHistory from '$lib/features/library/components/AlbumHistory.svelte';
	import AlbumRetagPairs from '$lib/features/library-management/components/AlbumRetagPairs.svelte';
	import {
		getLibraryAlbumCopiesQuery,
		getLibraryAlbumDetailQuery,
		getLibraryAlbumStatusQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import LocalAlbumPage from './LocalAlbumPage.svelte';
	import {
		getRatingsQuery,
		ratingOf,
		createSetRatingMutation
	} from '$lib/features/library/queries/ratings.svelte';
	import { createSetFavoriteMutation } from '$lib/features/library/queries/FavoriteMutations.svelte';

	const integrations = useIntegrationStatus();

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
	const rate = createSetRatingMutation();
	const heart = createSetFavoriteMutation();
	const primaryError = $derived(state.primaryError);
	const albumNotFound = $derived(primaryError instanceof ApiError && primaryError.status === 404);
	const providerUnavailable = $derived.by(() => {
		if (!(primaryError instanceof ApiError)) return false;
		return primaryError.status === 0 || primaryError.status === 429 || primaryError.status >= 500;
	});

	// The hero's Play is the first source bar's Play All: local files first,
	// then the media servers in the bars' order.
	const heroPlay = $derived.by(() => {
		const current = integrations.current;
		const local = state.localMatchForPlayback;
		if (current.localfiles && local?.found && local.tracks.length > 0) {
			return state.localCallbacks.onPlayAll;
		}
		const servers = [
			[current.jellyfin, state.jellyfinMatch, state.jellyfinCallbacks],
			[current.navidrome, state.navidromeMatch, state.navidromeCallbacks],
			[current.plex, state.plexMatch, state.plexCallbacks]
		] as const;
		for (const [enabled, match, callbacks] of servers) {
			if (enabled && match?.found && match.tracks.length > 0) return callbacks.onPlayAll;
		}
		return undefined;
	});

	$effect(() => {
		const canonicalId = state.album?.musicbrainz_id;
		if (canonicalId && canonicalId !== data.albumId) {
			void goto(albumHref(canonicalId), { replaceState: true });
		}
	});
</script>

{#snippet back()}
	<BackButton />
{/snippet}

<div class="w-full">
	{#if albumNotFound}
		<div class="dn-album-page">
			{@render back()}
			<div class="flex min-h-[50vh] items-center justify-center">
				<div class="alert alert-error">
					<span>Album not found.</span>
				</div>
			</div>
		</div>
	{:else if providerUnavailable && fallbackAlbum}
		<div class="dn-album-page pb-0">
			<div class="alert alert-info text-sm">
				<span
					>MusicBrainz is unreachable right now, so this page is built from your local files. Some
					extras are hidden until it returns.</span
				>
			</div>
		</div>
		<LocalAlbumPage albumId={fallbackAlbum.id} />
	{:else if providerUnavailable}
		<div class="dn-album-page">
			{@render back()}
			<div class="flex min-h-[50vh] items-center justify-center">
				<div class="alert alert-error">
					<span>MusicBrainz is temporarily unavailable.</span>
					<Button variant="ghost" size="sm" onclick={() => void state.refreshAll()}>Retry</Button>
				</div>
			</div>
		</div>
	{:else if state.error || primaryError}
		<div class="dn-album-page">
			{@render back()}
			<div class="flex min-h-[50vh] items-center justify-center">
				<div class="alert alert-error">
					<span>{state.error ?? 'Error loading album'}</span>
				</div>
			</div>
		</div>
	{:else if state.loadingBasic || !state.album}
		<AlbumHeroSkeleton tracks />
	{:else if state.album}
		{@const album = state.album}
		<AlbumHeader
			{album}
			{back}
			tracksInfo={state.tracksInfo}
			loadingTracks={state.loadingTracks}
			inLibrary={state.inLibrary}
			isRequested={state.isRequested}
			requesting={state.requesting}
			refreshing={state.refreshing}
			headerDownloadTask={state.headerDownloadTask}
			heldFiles={state.headerHeldFiles}
			downloadClientConfigured={integrations.current.download_client}
			libraryInLibrary={state.libraryInLibrary}
			libraryTrackCount={state.libraryTrackCount}
			libraryBelowCutoff={state.libraryBelowCutoff}
			editionCounts={state.editionCounts}
			mbTrackCount={state.tracksInfo?.total_tracks ?? 0}
			releaseGroupMbid={album.musicbrainz_id}
			{localCopies}
			editionPick={state.editionPick}
			downloadAllowed={state.downloadAllowed}
			rating={ratingOf(ratingsQuery.data, 'album', localAlbumId)}
			onrate={localAlbumId
				? (rating) => rate.mutate({ kind: 'album', itemId: localAlbumId, rating })
				: undefined}
			favorite={localAlbumQuery.data?.favorite ?? false}
			onfavorite={localAlbumQuery.data
				? (favorited) => heart.mutate({ kind: 'album', itemId: localAlbumId, favorited })
				: undefined}
			onplay={heroPlay}
			formats={state.libraryInLibrary
				? (state.localMatch?.primary_format?.toUpperCase() ?? null)
				: null}
			onrequest={state.handleRequest}
			ondelete={state.handleDeleteClick}
			onrefresh={state.refreshAll}
			onartistclick={state.goToArtist}
		/>

		<div class="dn-album-page space-y-8 pt-0">
			{#if localCopies.length > 1}
				<Card variant="bordered" padding="md" as="section" aria-label="Copies in your library">
					<h2 class="text-lg font-bold">Copies in your library</h2>
					<p class="mt-1 max-w-2xl text-sm text-muted">
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

			<section class="space-y-3" aria-labelledby="album-tracks-title">
				<div class="flex flex-wrap items-center justify-between gap-2">
					<h2 id="album-tracks-title" class="text-2xl text-base-content sm:text-3xl">Tracks</h2>
					{#if state.quota}
						<div class="flex items-center gap-2">
							<progress
								class="progress progress-accent h-1.5 w-20"
								value={state.quota.used}
								max={state.quota.limit}
							></progress>
							<span class="text-xs text-muted">{state.quota.remaining}/{state.quota.limit}</span>
						</div>
					{/if}
				</div>
				{#if state.loadingTracks}
					<PaperPanel>
						<ul class="divide-y divide-paper-border" aria-busy="true">
							{#each Array(8) as _, i (`track-skeleton-${i}`)}
								<li class="flex items-center gap-4 px-4 py-3">
									<div class="skeleton h-4 w-6"></div>
									<div class="skeleton h-4 flex-1"></div>
									<div class="skeleton h-4 w-12"></div>
								</li>
							{/each}
						</ul>
					</PaperPanel>
				{:else if state.tracksInfo && state.tracksInfo.tracks.length > 0}
					{@const tracksInfo = state.tracksInfo}
					<PaperPanel>
						{#snippet head()}
							<AlbumSourceBars
								{album}
								{tracksInfo}
								trackLinks={state.trackLinks}
								albumLink={state.albumLink}
								jellyfinMatch={state.jellyfinMatch}
								localMatch={state.localMatchForPlayback}
								keptCount={state.editionCounts?.kept}
								navidromeMatch={state.navidromeMatch}
								plexMatch={state.plexMatch}
								loadingJellyfin={state.loadingJellyfin}
								loadingLocal={state.loadingLocal}
								loadingNavidrome={state.loadingNavidrome}
								loadingPlex={state.loadingPlex}
								youtubeEnabled={integrations.current.youtube}
								youtubeApiConfigured={integrations.current.youtube_api}
								jellyfinEnabled={integrations.current.jellyfin}
								localfilesEnabled={integrations.current.localfiles}
								navidromeEnabled={integrations.current.navidrome}
								plexEnabled={integrations.current.plex}
								jellyfinCallbacks={state.jellyfinCallbacks}
								localCallbacks={state.localCallbacks}
								localDownloadCallback={state.localDownloadCallback}
								navidromeCallbacks={state.navidromeCallbacks}
								plexCallbacks={state.plexCallbacks}
								onTrackLinksUpdate={state.handleTrackLinksUpdate}
								onAlbumLinkUpdate={state.handleAlbumLinkUpdate}
								onQuotaUpdate={state.handleQuotaUpdate}
							/>
						{/snippet}
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
							youtubeEnabled={integrations.current.youtube}
							youtubeApiConfigured={integrations.current.youtube_api}
							jellyfinEnabled={integrations.current.jellyfin}
							localfilesEnabled={integrations.current.localfiles}
							navidromeEnabled={integrations.current.navidrome}
							plexEnabled={integrations.current.plex}
							libraryTracksByRecording={state.libraryTracksByRecording}
							libraryTracksByPosition={state.libraryTracksByPosition}
							heldByRecording={state.heldByRecording}
							heldByPosition={state.heldByPosition}
							trackDownloadTasks={state.trackDownloadTasks}
							editionRows={state.editionRows}
							releaseGroupMbid={album.musicbrainz_id}
							releaseMbid={tracksInfo.selected_release_mbid}
							trackRatings={ratingsQuery.data?.tracks}
							onRateTrack={(trackId, rating) =>
								rate.mutate({ kind: 'track', itemId: trackId, rating })}
							onFavoriteTrack={(trackId, favorited) =>
								heart.mutate({ kind: 'track', itemId: trackId, favorited })}
							onPlaySourceTrack={state.playSourceTrack}
							onTrackGenerated={state.handleTrackGenerated}
							onQuotaUpdate={state.handleQuotaUpdate}
							getTrackContextMenuItems={state.getTrackContextMenuItems}
						/>
					</PaperPanel>
					<AddToPlaylistModal bind:this={state.playlistModalRef} />
				{:else if state.tracksError}
					<div class="alert alert-warning">
						<span>Couldn't load the track list.</span>
						<Button variant="ghost" size="sm" onclick={state.retryTracks}>Retry</Button>
					</div>
				{:else}
					<div class="alert alert-warning">
						<span>No tracks available.</span>
						<Button variant="ghost" size="sm" onclick={state.retryTracks}>Retry</Button>
					</div>
				{/if}
				{#if album.release_date}
					<p class="text-xs text-muted">
						<span class="font-semibold">Release Date:</span>
						{album.release_date}
					</p>
				{/if}
			</section>

			{#if state.tracksInfo && state.tracksInfo.tracks.length > 0}
				<UnmatchedFilesSection
					files={state.libraryUnmatchedFiles}
					albumMbid={album.musicbrainz_id}
					albumId={localAlbumId}
					canRemove={authStore.isTrusted}
				/>
			{/if}

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

			{#if authStore.isAdmin && localAlbumId}
				<AlbumRetagPairs albumId={localAlbumId} />
			{/if}

			{#if localAlbumId && state.tracksInfo && state.tracksInfo.tracks.length > 0}
				<AlbumHistory albumId={localAlbumId} />
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
		<div class="dn-album-page flex min-h-[50vh] items-center justify-center">
			<p class="text-muted">Album not found</p>
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

<style>
	.dn-album-page {
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding: 1rem 1rem 2rem;
	}
	@media (min-width: 768px) {
		.dn-album-page {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-album-page {
			padding-inline: 2rem;
		}
	}
</style>
