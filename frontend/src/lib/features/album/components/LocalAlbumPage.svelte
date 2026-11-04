<script lang="ts">
	import { goto } from '$app/navigation';
	import { Check, Disc3, FileUp, FolderTree, Play } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import NoFilesBadge from '$lib/features/library/components/NoFilesBadge.svelte';
	import LocalIdentityBadge from '$lib/features/library/components/LocalIdentityBadge.svelte';
	import ManagementHoldSection from '$lib/features/library-management/components/ManagementHoldSection.svelte';
	import RecycledRemovals from '$lib/features/library-management/components/RecycledRemovals.svelte';
	import AlbumHistory from '$lib/features/library/components/AlbumHistory.svelte';
	import AlbumRetagPairs from '$lib/features/library-management/components/AlbumRetagPairs.svelte';
	import EditionPlate from '$lib/features/library/components/edition/EditionPlate.svelte';
	import AlbumOrganizationDialog from '$lib/features/library-management/components/AlbumOrganizationDialog.svelte';
	import LocalAlbumTrackList from '$lib/features/library/components/LocalAlbumTrackList.svelte';
	import LocalFilesIcon from '$lib/media/icons/LocalFilesIcon.svelte';
	import AlbumDownloadButton from './AlbumDownloadButton.svelte';
	import AlbumHero from './AlbumHero.svelte';
	import AlbumHeroSkeleton from './AlbumHeroSkeleton.svelte';
	import AlbumSourceBar from './AlbumSourceBar.svelte';
	import BackButton from '$lib/ui/BackButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ContextMenu, { type MenuItem } from '$lib/ui/ContextMenu.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { formatTotalDurationSec } from '$lib/utils/formatting';
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
	import { getTrackContextMenuItems } from './albumPlaybackHandlers';
	import type { QueueItem } from '$lib/features/player/playback/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/features/player/playback/queueHelpers';
	import {
		getLibraryAlbumDetailQuery,
		getLibraryAlbumTracksQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import type { AlbumBasicInfo, LocalTrackInfo } from '$lib/features/album/queries/types';
	import type { TrackView } from '$lib/features/library/queries/types';
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
	const downloadAllowed = $derived(downloadAccessQuery.data?.allowed === true);
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

	const formatLabel = $derived.by(() => {
		const format = album?.format?.toLowerCase();
		if (!format) return null;
		return format === 'mixed' ? 'Mixed formats' : format.toUpperCase();
	});

	let organization = $state<{
		start: (action: 'split' | 'merge' | 'move' | 'reset') => void;
	} | null>(null);

	// The hero's menu: Album organization's four actions for admins, and the
	// MusicBrainz contribution for a curator on a local-only album.
	const menuItems = $derived.by(() => {
		const items: MenuItem[] = [];
		if (authStore.isAdmin) {
			items.push(
				{ label: 'Split album...', icon: FolderTree, onclick: () => organization?.start('split') },
				{
					label: 'Merge with another local album...',
					icon: FolderTree,
					onclick: () => organization?.start('merge')
				},
				{ label: 'Move tracks...', icon: FolderTree, onclick: () => organization?.start('move') },
				{
					label: 'Reset manual grouping...',
					icon: FolderTree,
					onclick: () => organization?.start('reset')
				}
			);
		}
		if (authStore.isTrusted && album?.identity_state === 'local_only') {
			items.push({
				label: album.contribution_id ? 'Contribution in progress' : 'Contribute to MusicBrainz',
				icon: FileUp,
				disabled: contributionMutation.isPending,
				onclick: openContribution
			});
		}
		return items;
	});

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

{#snippet back()}
	<BackButton fallback="/library/albums" />
{/snippet}

<div class="w-full">
	{#if albumQuery.isLoading}
		<AlbumHeroSkeleton tracks />
	{:else if albumQuery.isError || !album}
		<div class="dn-album-page">
			{@render back()}
			<div class="alert alert-error mt-4">
				<span>Couldn't load this album.</span>
				<Button size="sm" onclick={() => albumQuery.refetch()}>Retry</Button>
			</div>
		</div>
	{:else}
		<AlbumHero eyebrow={album.is_compilation ? 'Compilation' : 'Album'} title={album.title} {back}>
			{#snippet cover()}
				<AlbumImage
					albumId={album.id}
					mbid={album.release_group_mbid ?? ''}
					available={album.cover_available}
					artMissing={album.art_missing}
					alt={`Album cover for ${album.title} by ${album.artist_name || 'Unknown album artist'}`}
					size="full"
					requestSize={500}
					priority
					rounded="none"
					className="aspect-square w-full"
				/>
			{/snippet}

			{#snippet meta()}
				<a href={artistHref(album.artist_mbid ?? album.artist_id)} class="font-bold hover:underline"
					>{album.artist_name || 'Unknown album artist'}</a
				>
				<span class="opacity-50" aria-hidden="true">•</span>
				<span>{album.year ?? 'Year unknown'}</span>
				<span class="opacity-50" aria-hidden="true">•</span>
				<span>{album.track_count} {album.track_count === 1 ? 'track' : 'tracks'}</span>
				{#if album.total_duration_seconds}
					<span class="opacity-50" aria-hidden="true">•</span>
					<span>{formatTotalDurationSec(album.total_duration_seconds)}</span>
				{/if}
				{#if hasFiles && formatLabel}
					<span class="opacity-50" aria-hidden="true">•</span>
					<span>{formatLabel}</span>
				{/if}
				<span class="flex items-center">
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
				</span>
			{/snippet}

			{#snippet details()}
				<div class="flex flex-wrap items-center gap-2">
					{#if !hasFiles}<NoFilesBadge />{/if}
					<LocalIdentityBadge
						state={album.identity_state}
						releaseMbid={album.release_mbid}
						subject="album"
						showDescription
					/>
				</div>
				<EditionPlate copies={[album]} />
			{/snippet}

			{#snippet actions()}
				{#if hasFiles}
					<Button
						variant="primary"
						icon={Play}
						class="min-w-28 [&_svg]:fill-current"
						disabled={!tracks.length}
						onclick={() => play(false)}>Play</Button
					>
					<StatusChip size="md" icon={Check}>In Library</StatusChip>
					<AlbumDownloadButton
						albumId={album.id}
						mbid={null}
						totalSizeBytes={album.total_size_bytes}
						trackCount={tracks.length}
						{downloadAllowed}
					/>
				{/if}
				{#if menuItems.length > 0}
					<ContextMenu items={menuItems} trigger="button" label="More album actions" />
				{/if}
			{/snippet}
		</AlbumHero>

		<div class="dn-album-page space-y-8 pt-0">
			<section class="space-y-3" aria-labelledby="local-tracks-title">
				<h2 id="local-tracks-title" class="text-2xl text-base-content sm:text-3xl">Tracks</h2>
				{#if tracksQuery.isLoading}
					<PaperPanel>
						<ul class="divide-y divide-paper-border" aria-busy="true">
							{#each Array(8) as _, index (index)}
								<li class="flex items-center gap-4 px-4 py-3">
									<div class="skeleton h-4 w-6"></div>
									<div class="skeleton h-4 flex-1"></div>
									<div class="skeleton h-4 w-12"></div>
								</li>
							{/each}
						</ul>
					</PaperPanel>
				{:else if tracksQuery.isError}
					<div class="alert alert-error">Couldn't load tracks.</div>
				{:else if !tracks.length}
					<Card class="text-center text-muted">
						<Disc3 class="mx-auto h-8 w-8 opacity-30" />
						<p>No playable tracks are attached to this album.</p>
					</Card>
				{:else}
					<PaperPanel>
						{#snippet head()}
							<AlbumSourceBar
								sourceLabel="Local Files"
								sourceColor="var(--color-accent)"
								trackCount={tracks.length}
								totalTracks={tracks.length}
								extraBadge={formatLabel}
								onPlayAll={() => play(false)}
								onShuffle={() => play(true)}
							>
								{#snippet icon()}
									<LocalFilesIcon class="h-5 w-5" />
								{/snippet}
							</AlbumSourceBar>
						{/snippet}
						<LocalAlbumTrackList
							{tracks}
							framed={false}
							notes={fileNotes}
							getTrackMenuItems={getLocalTrackMenuItems}
							trackRatings={ratingsQuery.data?.tracks}
							onRateTrack={(trackId, rating) =>
								rate.mutate({ kind: 'track', itemId: trackId, rating })}
							onFavoriteTrack={(trackId, favorited) =>
								heart.mutate({ kind: 'track', itemId: trackId, favorited })}
						/>
					</PaperPanel>
				{/if}
			</section>

			{#if authStore.isAdmin}
				<ManagementHoldSection {album} />
			{/if}

			{#if authStore.isTrusted}
				<RecycledRemovals albumId={album.id} releaseGroupMbid={album.release_group_mbid} />
			{/if}

			{#if authStore.isAdmin}
				<AlbumRetagPairs {albumId} />
			{/if}

			<AlbumHistory {albumId} />
		</div>

		{#if authStore.isAdmin}
			<AlbumOrganizationDialog bind:this={organization} {album} {tracks} />
		{/if}
		<AddToPlaylistModal bind:this={playlistModal} />
	{/if}
</div>

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
