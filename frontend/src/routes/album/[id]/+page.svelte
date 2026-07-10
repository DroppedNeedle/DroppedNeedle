<script lang="ts">
	import { integrationStore } from '$lib/stores/integration';
	import BackButton from '$lib/components/BackButton.svelte';
	import Toast from '$lib/components/Toast.svelte';
	import LastFmAlbumEnrichmentComponent from '$lib/components/LastFmAlbumEnrichment.svelte';
	import DeleteAlbumModal from '$lib/components/DeleteAlbumModal.svelte';
	import ArtistRemovedModal from '$lib/components/ArtistRemovedModal.svelte';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import { createAlbumPageState } from './albumPageState.svelte';
	import AlbumHeader from './AlbumHeader.svelte';
	import UnmatchedFilesSection from './UnmatchedFilesSection.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AlbumSourceBars from './AlbumSourceBars.svelte';
	import AlbumTrackList from './AlbumTrackList.svelte';
	import AlbumDiscovery from './AlbumDiscovery.svelte';
	import WhereToBuy from './WhereToBuy.svelte';
	import { CalendarDays, ListMusic } from 'lucide-svelte';

	let { data }: { data: { albumId: string } } = $props();

	const state = createAlbumPageState(() => data.albumId);
</script>

<div class="w-full px-2 sm:px-4 lg:px-8 py-4 sm:py-8 max-w-7xl mx-auto">
	{#if state.error}
		<div class="mb-4">
			<BackButton />
		</div>
		<div class="flex items-center justify-center min-h-[50vh]">
			<div
				class="flex items-center gap-3 rounded-2xl border border-error/30 bg-error/10 px-5 py-4 text-sm text-error"
			>
				<span>{state.error}</span>
			</div>
		</div>
	{:else if state.loadingBasic || !state.album}
		<div class="mb-4">
			<BackButton />
		</div>
		<div class="space-y-6 sm:space-y-8">
			<div class="flex flex-col lg:flex-row gap-6 lg:gap-8">
				<div class="skeleton w-full lg:w-64 xl:w-80 aspect-square rounded-2xl shrink-0"></div>
				<div class="flex-1 flex flex-col justify-end space-y-4">
					<div class="skeleton h-6 w-24 rounded-full"></div>
					<div class="skeleton h-12 w-3/4"></div>
					<div class="skeleton h-6 w-1/2"></div>
					<div class="flex gap-4 mt-6">
						<div class="skeleton h-12 w-32 rounded-full"></div>
						<div class="skeleton h-12 w-32 rounded-full"></div>
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
				downloadClientConfigured={$integrationStore.download_client}
				libraryInLibrary={state.libraryInLibrary}
				libraryTrackCount={state.libraryTrackCount}
				libraryBelowCutoff={state.libraryBelowCutoff}
				coverageExpected={state.coverageExpected}
				coverageCovered={state.coverageCovered}
				mbTrackCount={state.tracksInfo?.total_tracks ?? 0}
				releaseGroupMbid={album.musicbrainz_id}
				onrequest={state.handleRequest}
				ondelete={state.handleDeleteClick}
				onrefresh={state.refreshAll}
				onartistclick={state.goToArtist}
			/>

			<WhereToBuy releaseGroupMbid={album.musicbrainz_id} />

			{#if state.loadingTracks}
				<div class="space-y-3">
					<h2
						class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<ListMusic class="h-4 w-4 text-accent" />
						Tracks
					</h2>
					<div class="overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50">
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
						<h2
							class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
						>
							<ListMusic class="h-4 w-4 text-accent" />
							Tracks
						</h2>
						{#if state.quota}
							<div class="flex items-center gap-2">
								<progress
									class="progress progress-accent w-20 h-1.5"
									value={state.quota.used}
									max={state.quota.limit}
								></progress>
								<span class="font-mono text-xs tabular-nums text-base-content/50"
									>{state.quota.remaining}/{state.quota.limit}</span
								>
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
						releaseGroupMbid={album.musicbrainz_id}
						onPlaySourceTrack={state.playSourceTrack}
						onTrackGenerated={state.handleTrackGenerated}
						onQuotaUpdate={state.handleQuotaUpdate}
						getTrackContextMenuItems={state.getTrackContextMenuItems}
					/>

					<UnmatchedFilesSection
						orphans={state.libraryOrphans}
						albumMbid={album.musicbrainz_id}
						canRemove={authStore.isTrusted}
					/>

					<AddToPlaylistModal bind:this={state.playlistModalRef} />
				</div>
			{:else if state.tracksError}
				<div class="space-y-3">
					<h2
						class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<ListMusic class="h-4 w-4 text-accent" />
						Tracks
					</h2>
					<div
						class="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-warning/30 bg-warning/5 px-4 py-3 text-sm"
					>
						<span class="text-base-content/70">Couldn't load the track list.</span>
						<button class="btn btn-sm btn-ghost rounded-full" onclick={state.retryTracks}>
							Retry
						</button>
					</div>
				</div>
			{:else}
				<div class="space-y-3">
					<h2
						class="flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<ListMusic class="h-4 w-4 text-accent" />
						Tracks
					</h2>
					<div
						class="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-warning/30 bg-warning/5 px-4 py-3 text-sm"
					>
						<span class="text-base-content/70">No tracks available.</span>
						<button class="btn btn-sm btn-ghost rounded-full" onclick={state.retryTracks}>
							Retry
						</button>
					</div>
				</div>
			{/if}

			{#if album.release_date}
				<div
					class="flex items-center gap-2 font-mono text-[0.65rem] uppercase tracking-[0.14em] text-base-content/45"
				>
					<CalendarDays class="h-3.5 w-3.5 text-accent" />
					<span class="font-bold text-base-content/35">Release Date</span>
					{album.release_date}
				</div>
			{/if}

			{#if state.loadingLastfm || state.lastfmEnrichment}
				<LastFmAlbumEnrichmentComponent
					enrichment={state.lastfmEnrichment}
					loading={state.loadingLastfm}
				/>
			{/if}

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

<Toast bind:show={state.showToast} message={state.toastMessage} type={state.toastType} />

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

{#if state.showArtistRemovedModal}
	<ArtistRemovedModal
		artistName={state.removedArtistName}
		onclose={() => {
			state.showArtistRemovedModal = false;
		}}
	/>
{/if}
