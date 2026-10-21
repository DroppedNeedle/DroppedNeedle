<script lang="ts">
	import { goto } from '$app/navigation';
	import { ChevronLeft, Disc3, FileUp, ListMusic, Play, Shuffle } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import LibraryFormatBadge from '$lib/components/library/LibraryFormatBadge.svelte';
	import NoFilesBadge from '$lib/components/library/NoFilesBadge.svelte';
	import LocalIdentityBadge from '$lib/components/library/LocalIdentityBadge.svelte';
	import ManagementHoldSection from '$lib/components/library/ManagementHoldSection.svelte';
	import RecycledRemovals from '$lib/components/library/RecycledRemovals.svelte';
	import AlbumHistory from '$lib/components/library/AlbumHistory.svelte';
	import RetagPairCard from '$lib/components/library/RetagPairCard.svelte';
	import EditionPlate from '$lib/components/library/edition/EditionPlate.svelte';
	import AlbumOrganizationDialog from '$lib/components/library/AlbumOrganizationDialog.svelte';
	import LocalAlbumTrackList from '$lib/components/library/LocalAlbumTrackList.svelte';
	import AlbumDownloadButton from './AlbumDownloadButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import { getRatingsQuery, ratingOf, setRating } from '$lib/queries/ratings/RatingQueries.svelte';
	import { getDownloadAccessQuery } from '$lib/queries/local/DownloadAccessQuery.svelte';
	import {
		getAlbumEditionStatusQuery,
		getEditionPlanQuery
	} from '$lib/queries/albums/EditionQueries.svelte';
	import { getRetagPairsQuery } from '$lib/queries/library/LibraryRetagPairQueries.svelte';
	import { notesByFile } from './albumEdition';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import { getTrackContextMenuItems } from './albumPlaybackHandlers';
	import type { QueueItem } from '$lib/player/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import {
		getLibraryAlbumDetailQuery,
		getLibraryAlbumTracksQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import type {
		AlbumBasicInfo,
		LocalTrackInfo,
		NativePickBasis,
		NativeTrackListItem
	} from '$lib/types';
	import { createLibraryContributionMutation } from '$lib/queries/libraryContributions/LibraryContributionMutations.svelte';
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
	const downloadAllowed = $derived(
		album?.download_allowed !== false && downloadAccessQuery.data?.allowed !== false
	);
	const contributionMutation = createLibraryContributionMutation();

	// One ratings read for the album and its tracks, once the tracks are in.
	const ratingsQuery = getRatingsQuery(() =>
		album && tracksQuery.isSuccess
			? { albumIds: [album.id], trackIds: tracks.map((track) => track.id) }
			: null
	);
	const rate = setRating();

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

	const retagPairsQuery = getRetagPairsQuery(() => authStore.isAdmin);
	const retagPairs = $derived(
		(retagPairsQuery.data?.pairs ?? []).filter(
			(pair) => pair.old_album.id === albumId || pair.new_album.id === albumId
		)
	);

	const reviewLabel = $derived(
		album?.identification_status === 'needs_review'
			? 'Needs review'
			: album?.identification_status === 'keep_tagged'
				? 'Keep as tagged'
				: album?.identification_status === 'manual_identity_needs_review'
					? 'Manual identity needs review'
					: null
	);
	// Per-basis qualifier copy; an unknown future basis shows no qualifier
	// (fail-closed, mirroring LocalIdentityBadge).
	const pickBasisQualifier: Record<NativePickBasis, string> = {
		pin: 'Year and cover shown from your pinned pressing.',
		owned: 'Year and cover shown from your identified edition.',
		embedded_tags: 'Year and cover shown from the best-fit pressing until the edition is verified.'
	};
	const qualifierText = $derived(
		album?.album_identity_state === 'release_group_linked' && album.pick_basis
			? (pickBasisQualifier[album.pick_basis] ?? null)
			: null
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
	function getLocalTrackMenuItems(track: NativeTrackListItem): MenuItem[] {
		const albumForMenu: AlbumBasicInfo = {
			title: track.album_title,
			musicbrainz_id: track.musicbrainz_release_group_id ?? track.album_id,
			artist_name: track.artist_name,
			artist_id: track.artist_id,
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
				mbid={album.musicbrainz_release_group_id ?? ''}
				available={album.cover_available}
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
					{#if reviewLabel}<span class="badge badge-warning badge-outline">{reviewLabel}</span>{/if}
				</div>
				<h1 class="mt-3 text-3xl font-black tracking-tight sm:text-5xl">{album.title}</h1>
				<a
					href={artistHref(album.musicbrainz_artist_id ?? album.artist_id)}
					class="mt-2 inline-block text-lg text-base-content/65 hover:underline"
					>{album.artist_name || 'Unknown album artist'}</a
				>
				<LocalIdentityBadge
					state={album.album_identity_state}
					subject="album"
					showDescription
					className="mt-3"
					pickBasis={album.pick_basis ?? null}
				/>
				<p class="mt-2 text-sm text-base-content/50">
					{album.year ?? 'Year unknown'} · {album.track_count}
					{album.track_count === 1 ? 'track' : 'tracks'}
					{#if qualifierText}
						<span
							class="ml-1 align-middle text-xs text-base-content/45"
							title={qualifierText}
							aria-label={qualifierText}>· best-fit edition</span
						>
					{/if}
				</p>
				<RatingStars
					size="md"
					class="mt-2 -ml-1"
					value={ratingOf(ratingsQuery.data, 'album', album.id)}
					label={album.title}
					onchange={(rating) => rate.mutate({ kind: 'album', itemId: album.id, rating })}
				/>
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
					{#if authStore.isTrusted && album.album_identity_state === 'local_only'}
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
				{#if album.review_id && authStore.isAdmin}
					<a
						class="link link-warning mt-3 inline-block text-sm"
						href={withBasePath(`/library/review?review=${album.review_id}`)}
						>Open identification review</a
					>
				{/if}
			</div>
		</header>

		{#if authStore.isAdmin}
			<div class="mt-6"><ManagementHoldSection {album} /></div>
		{/if}

		{#if authStore.isTrusted}
			<RecycledRemovals
				class="mt-6"
				albumId={album.id}
				releaseGroupMbid={album.musicbrainz_release_group_id}
			/>
		{/if}

		{#each retagPairs as pair (`${pair.old_album.id}/${pair.new_album.id}`)}
			<div class="mt-6"><RetagPairCard {pair} currentAlbumId={albumId} /></div>
		{/each}

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
				/>
			{/if}
		</section>

		<AlbumHistory {albumId} />

		<AddToPlaylistModal bind:this={playlistModal} />
	{/if}
</main>
