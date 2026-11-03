<script lang="ts">
	import { goto } from '$app/navigation';
	import { ChevronLeft, Disc3, FileUp, ListMusic, Play, Shuffle } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import LibraryFormatBadge from '$lib/features/library/components/LibraryFormatBadge.svelte';
	import NoFilesBadge from '$lib/features/library/components/NoFilesBadge.svelte';
	import LocalIdentityBadge from '$lib/features/library/components/LocalIdentityBadge.svelte';
	import ManagementHoldSection from '$lib/features/library-management/components/ManagementHoldSection.svelte';
	import RecycledRemovals from '$lib/features/library-management/components/RecycledRemovals.svelte';
	import AlbumHistory from '$lib/features/library/components/AlbumHistory.svelte';
	import AlbumRetagPairs from '$lib/features/library-management/components/AlbumRetagPairs.svelte';
	import EditionPlate from '$lib/features/library/components/edition/EditionPlate.svelte';
	import AlbumOrganizationDialog from '$lib/features/library-management/components/AlbumOrganizationDialog.svelte';
	import LocalAlbumTrackList from '$lib/features/library/components/LocalAlbumTrackList.svelte';
	import AlbumDownloadButton from './AlbumDownloadButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import {
		getRatingsQuery,
		ratingOf,
		createSetRatingMutation
	} from '$lib/features/library/queries/ratings.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';
	import { createSetFavoriteMutation } from '$lib/features/library/queries/FavoriteMutations.svelte';
	import { getDownloadAccessQuery } from '$lib/features/album/queries/DownloadAccessQuery.svelte';
	import {
		getAlbumEditionStatusQuery,
		getEditionPlanQuery
	} from '$lib/features/library/queries/EditionQueries.svelte';
	import { notesByFile } from './albumEdition';
	import AddToPlaylistModal from '$lib/features/playlists/components/AddToPlaylistModal.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { getTrackContextMenuItems } from './albumPlaybackHandlers';
	import type { QueueItem } from '$lib/features/player/playback/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/features/player/playback/queueHelpers';
	import {
		getLibraryAlbumDetailQuery,
		getLibraryAlbumTracksQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import type { AlbumBasicInfo, LocalTrackInfo, TrackView } from '$lib/types';
	import { createLibraryContributionMutation } from '$lib/features/library/queries/LibraryContributionMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { artistHref } from '$lib/utils/entityRoutes';

	interface Props {
		albumId: string;
	}

	let { albumId }: Props = $props();
	const albumQuery = getLibraryAlbumDetailQuery(() => albumId);
	const tracksQuery = getLibraryAlbumTracksQuery(() => albumId);
	const album = $derived(albumQuery.data);
	const tracks = $derived(tracksQuery.data?.items ?? []);
	// The track count is over files in the library: none means nothing to play.
	const hasFiles = $derived((album?.track_count ?? 0) > 0);
	// The download buttons follow the admin's "Library downloads" setting.
	const downloadAccessQuery = getDownloadAccessQuery(() => authStore.user?.id);
	const downloadAllowed = $derived(downloadAccessQuery.data?.allowed !== false);
	const contributionMutation = createLibraryContributionMutation();

	// One ratings read for the album and its tracks, once the tracks are in.
	const ratingsQuery = getRatingsQuery(() =>
		album && tracksQuery.isSuccess
			? { albumIds: [album.id], trackIds: tracks.map((track) => track.id) }
			: null
	);
	const rate = createSetRatingMutation();
	const heart = createSetFavoriteMutation();

	// The edition plan's word on single files, read under the plate's keys.
	const userId = () => authStore.user?.id;
	const editionStatusQuery = getAlbumEditionStatusQuery(userId, () => albumId);
	const editionPlanQuery = getEditionPlanQuery(
		userId,
		() => albumId,
		() => null,
		() =>
			editionStatusQuery.data?.album_id === albumId &&
			editionStatusQuery.data.edition_kind === 'release'
	);
	const fileNotes = $derived(
		editionPlanQuery.data?.album_id === albumId ? notesByFile(editionPlanQuery.data) : undefined
	);

	function play(shuffle: boolean): void {
		const queue = buildDiscoveryQueueFromLocal(tracks);
		if (!queue.length) return;
		playerStore.playQueue(queue, 0, shuffle);
	}

	let playlistModal = $state<{ open: (tracks: QueueItem[]) => void } | null>(null);

	// Same 4-item menu as provider rows, via the shared builder: the native
	// track adapts to the provider shapes (the backend download param is the
	// file id, which is track.id on this path).
	function getLocalTrackMenuItems(track: TrackView): MenuItem[] {
		const albumForMenu: AlbumBasicInfo = {
			title: track.album_title,
			musicbrainz_id: track.release_group_mbid ?? track.album_id,
			artist_name: track.artist_name,
			artist_id: track.artist_id ?? '',
			in_library: true
		};
		const resolvedLocal: LocalTrackInfo = {
			track_file_id: track.id,
			title: track.title,
			track_number: track.track_number,
			disc_number: track.disc_number,
			duration_seconds: track.duration_seconds,
			size_bytes: track.file_size_bytes,
			format: track.format
		};
		return getTrackContextMenuItems(
			{ position: track.track_number, disc_number: track.disc_number, title: track.title },
			albumForMenu,
			resolvedLocal,
			null,
			null,
			null,
			playlistModal,
			downloadAllowed
		);
	}

	function openContribution(): void {
		if (!album) return;
		if (album.contribution_id) {
			void goto(withBasePath(`/library/contributions/${album.contribution_id}`));
			return;
		}
		contributionMutation.mutate(album.id);
	}
</script>

<svelte:head><title>{album?.title ?? 'Album'} · Library</title></svelte:head>

<main class="container mx-auto p-4 md:p-6 lg:p-8">
	<Button
		variant="ghost"
		size="sm"
		icon={ChevronLeft}
		class="mb-5"
		onclick={() => goto(withBasePath('/library/albums'))}
	>
		Albums
	</Button>

	{#if albumQuery.isLoading}
		<div class="grid gap-6 md:grid-cols-[16rem_1fr]">
			<div class="skeleton aspect-square w-full rounded-2xl"></div>
			<div class="space-y-3">
				<div class="skeleton h-10 w-2/3"></div>
				<div class="skeleton h-5 w-1/3"></div>
				<div class="skeleton h-32 w-full"></div>
			</div>
		</div>
	{:else if albumQuery.isError || !album}
		<div class="alert alert-error">
			<span>Couldn't load this album.</span>
			<Button size="sm" onclick={() => albumQuery.refetch()}>Retry</Button>
		</div>
	{:else}
		<header class="grid gap-6 md:grid-cols-[16rem_1fr] md:items-end">
			<AlbumImage
				albumId={album.id}
				mbid={album.release_group_mbid ?? ''}
				available={album.cover_available}
				artMissing={album.art_missing}
				alt={`Cover for ${album.title}`}
				size="full"
				requestSize={500}
				priority
				className="aspect-square w-full rounded-2xl shadow-xl"
			/>
			<div class="min-w-0">
				<div class="flex flex-wrap items-center gap-2">
					{#if hasFiles}<LibraryFormatBadge format={album.format} />{:else}<NoFilesBadge />{/if}
					{#if album.is_compilation}<span class="badge badge-ghost">Compilation</span>{/if}
				</div>
				<h1 class="mt-3 text-3xl font-black tracking-tight sm:text-5xl">{album.title}</h1>
				<a
					href={artistHref(album.artist_mbid ?? album.artist_id)}
					class="mt-2 inline-block text-lg text-base-content/65 hover:underline"
					>{album.artist_name || 'Unknown album artist'}</a
				>
				<LocalIdentityBadge
					state={album.identity_state}
					releaseMbid={album.release_mbid}
					subject="album"
					showDescription
					className="mt-3"
				/>
				<p class="mt-2 text-sm text-base-content/50">
					{album.year ?? 'Year unknown'} · {album.track_count}
					{album.track_count === 1 ? 'track' : 'tracks'}
				</p>
				<div class="mt-2 -ml-1 flex items-center">
					<RatingStars
						size="md"
						value={ratingOf(ratingsQuery.data, 'album', album.id)}
						label={album.title}
						onchange={(rating) => rate.mutate({ kind: 'album', itemId: album.id, rating })}
					/>
					<FavoriteHeart
						size="md"
						favorite={album.favorite ?? false}
						onchange={(favorited) => heart.mutate({ kind: 'album', itemId: album.id, favorited })}
					/>
				</div>
				<div class="mt-4">
					<EditionPlate copies={[album]} />
				</div>
				<div class="mt-5 flex flex-wrap items-center gap-2">
					{#if hasFiles}
						<Button
							variant="primary"
							icon={Play}
							class="[&_svg]:fill-current"
							disabled={!tracks.length}
							onclick={() => play(false)}>Play</Button
						>
						<Button
							variant="ghost"
							icon={Shuffle}
							disabled={!tracks.length}
							onclick={() => play(true)}>Shuffle</Button
						>
						<AlbumDownloadButton
							albumId={album.id}
							mbid={null}
							totalSizeBytes={album.total_size_bytes}
							trackCount={tracks.length}
							{downloadAllowed}
						/>
					{/if}
					{#if authStore.isTrusted && album.identity_state === 'local_only'}
						<Button
							variant="ghost"
							icon={FileUp}
							disabled={contributionMutation.isPending}
							onclick={openContribution}
						>
							{album.contribution_id ? 'Contribution in progress' : 'Contribute to MusicBrainz'}
						</Button>
					{/if}
					{#if authStore.isAdmin}<AlbumOrganizationDialog {album} {tracks} />{/if}
				</div>
			</div>
		</header>

		{#if authStore.isAdmin}
			<div class="mt-6"><ManagementHoldSection {album} /></div>
		{/if}

		{#if authStore.isTrusted}
			<RecycledRemovals
				class="mt-6"
				albumId={album.id}
				releaseGroupMbid={album.release_group_mbid}
			/>
		{/if}

		{#if authStore.isAdmin}
			<AlbumRetagPairs {albumId} class="mt-6" />
		{/if}

		<section class="mt-8" aria-labelledby="local-tracks-title">
			<div class="flex items-center gap-2">
				<ListMusic class="h-5 w-5 text-primary" />
				<h2 id="local-tracks-title" class="text-xl font-bold">Tracks</h2>
			</div>
			{#if tracksQuery.isLoading}
				<div class="mt-3 space-y-2">
					{#each Array(8) as _, index (index)}<div class="skeleton h-14 w-full"></div>{/each}
				</div>
			{:else if tracksQuery.isError}
				<div class="alert alert-error mt-3">Couldn't load tracks.</div>
			{:else if !tracks.length}
				<Card class="mt-3 text-center text-base-content/55">
					<Disc3 class="mx-auto h-8 w-8 opacity-30" />
					<p>No playable tracks are attached to this album.</p>
				</Card>
			{:else}
				<LocalAlbumTrackList
					{tracks}
					notes={fileNotes}
					getTrackMenuItems={getLocalTrackMenuItems}
					trackRatings={ratingsQuery.data?.tracks}
					onRateTrack={(trackId, rating) => rate.mutate({ kind: 'track', itemId: trackId, rating })}
					onFavoriteTrack={(trackId, favorited) =>
						heart.mutate({ kind: 'track', itemId: trackId, favorited })}
				/>
			{/if}
		</section>

		<AlbumHistory {albumId} />

		<AddToPlaylistModal bind:this={playlistModal} />
	{/if}
</main>
