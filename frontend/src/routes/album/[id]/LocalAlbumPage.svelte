<script lang="ts">
	import { goto } from '$app/navigation';
	import {
		ChevronLeft,
		Disc3,
		ExternalLink,
		FileUp,
		ListMusic,
		Play,
		Shuffle
	} from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import LibraryFormatBadge from '$lib/components/library/LibraryFormatBadge.svelte';
	import LocalIdentityBadge from '$lib/components/library/LocalIdentityBadge.svelte';
	import AlbumIdentificationPanel from '$lib/components/library/AlbumIdentificationPanel.svelte';
	import AlbumOrganizationDialog from '$lib/components/library/AlbumOrganizationDialog.svelte';
	import LocalAlbumTrackList from '$lib/components/library/LocalAlbumTrackList.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { playerStore } from '$lib/stores/player.svelte';
	import { buildDiscoveryQueueFromLocal } from '$lib/player/queueHelpers';
	import {
		getLibraryAlbumDetailQuery,
		getLibraryAlbumTracksQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import { getAlbumEditionsQuery } from '$lib/queries/albums/EditionQueries.svelte';
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
	const editionsQuery = getAlbumEditionsQuery(
		() => authStore.user?.id,
		() => album?.musicbrainz_release_group_id ?? '',
		() => Boolean(album?.musicbrainz_release_group_id)
	);
	const editions = $derived(editionsQuery.data?.items ?? []);
	const contributionMutation = createLibraryContributionMutation();

	const reviewLabel = $derived(
		album?.identification_status === 'needs_review'
			? 'Needs review'
			: album?.identification_status === 'keep_tagged'
				? 'Keep as tagged'
				: album?.identification_status === 'manual_identity_needs_review'
					? 'Manual identity needs review'
					: null
	);
	const managementIdentityAttention = $derived(
		album?.management_identity_readiness === 'exact_release_required'
			? 'Exact MusicBrainz edition required'
			: album?.management_identity_readiness === 'track_mapping_required'
				? 'Exact track map required'
				: album?.management_identity_readiness === 'custom_manifest_stale'
					? 'Custom edition review required'
					: album?.identification_status === 'needs_review' ||
						  album?.identification_status === 'manual_identity_needs_review'
						? 'Identity review required'
						: null
	);
	const musicbrainzUrl = $derived(
		album?.musicbrainz_release_id
			? `https://musicbrainz.org/release/${album.musicbrainz_release_id}`
			: album?.musicbrainz_release_group_id
				? `https://musicbrainz.org/release-group/${album.musicbrainz_release_group_id}`
				: null
	);

	function play(shuffle: boolean): void {
		const queue = buildDiscoveryQueueFromLocal(tracks);
		if (!queue.length) return;
		playerStore.playQueue(queue, 0, shuffle);
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
	<button
		class="btn btn-ghost btn-sm mb-5 gap-2"
		onclick={() => goto(withBasePath('/library/albums'))}
	>
		<ChevronLeft class="h-4 w-4" /> Albums
	</button>

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
			<span>Couldn't load this album.</span><button
				class="btn btn-sm"
				onclick={() => albumQuery.refetch()}>Retry</button
			>
		</div>
	{:else}
		<header class="grid gap-6 md:grid-cols-[16rem_1fr] md:items-end">
			<AlbumImage
				mbid={album.id}
				source="local"
				available={album.cover_available}
				alt={`Cover for ${album.title}`}
				size="full"
				className="aspect-square w-full rounded-2xl shadow-xl"
			/>
			<div class="min-w-0">
				<div class="flex flex-wrap items-center gap-2">
					<LibraryFormatBadge format={album.format} />
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
				/>
				<p class="mt-2 text-sm text-base-content/50">
					{album.year ?? 'Year unknown'} · {album.track_count}
					{album.track_count === 1 ? 'track' : 'tracks'}
				</p>
				<div class="mt-5 flex flex-wrap items-center gap-2">
					<button
						class="btn btn-primary gap-2"
						disabled={!tracks.length}
						onclick={() => play(false)}><Play class="h-4 w-4 fill-current" /> Play</button
					>
					<button class="btn btn-ghost gap-2" disabled={!tracks.length} onclick={() => play(true)}
						><Shuffle class="h-4 w-4" /> Shuffle</button
					>
					{#if authStore.isTrusted && album.album_identity_state === 'local_only'}
						<button
							class="btn btn-ghost gap-2"
							disabled={contributionMutation.isPending}
							onclick={openContribution}
						>
							<FileUp class="h-4 w-4" />
							{album.contribution_id ? 'Contribution in progress' : 'Contribute to MusicBrainz'}
						</button>
					{/if}
					{#if authStore.isAdmin}<AlbumIdentificationPanel
							{album}
							attentionLabel={managementIdentityAttention}
						/><AlbumOrganizationDialog {album} {tracks} />{/if}
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
				<div class="mt-3 rounded-box bg-base-200 p-6 text-center text-base-content/55">
					<Disc3 class="mx-auto h-8 w-8 opacity-30" />
					<p class="mt-2">No playable tracks are attached to this album.</p>
				</div>
			{:else}
				<LocalAlbumTrackList {tracks} />
			{/if}
		</section>

		<section
			class="mt-6 rounded-box border border-base-content/10 bg-base-200/35 p-4"
			aria-labelledby="provider-features-title"
		>
			<div class="flex flex-wrap items-center justify-between gap-3">
				<h2 id="provider-features-title" class="font-semibold">MusicBrainz editions</h2>
				{#if musicbrainzUrl}
					<a
						class="btn btn-ghost btn-sm gap-2"
						href={musicbrainzUrl}
						target="_blank"
						rel="noreferrer">Open in MusicBrainz <ExternalLink class="h-3.5 w-3.5" /></a
					>
				{/if}
			</div>
			{#if album.musicbrainz_release_group_id}
				{#if editionsQuery.isLoading}
					<div class="mt-3 grid gap-2 sm:grid-cols-2">
						<div class="skeleton h-12"></div>
						<div class="skeleton h-12"></div>
					</div>
				{:else if editionsQuery.isError}
					<p class="mt-3 text-sm text-base-content/55">
						MusicBrainz edition details are temporarily unavailable.
					</p>
				{:else if editions.length}
					<ul class="mt-3 grid gap-2 sm:grid-cols-2">
						{#each editions.slice(0, 6) as edition (edition.release_mbid)}
							<li>
								<a
									class="flex h-full items-center justify-between gap-3 rounded-box border border-base-content/10 bg-base-100 px-3 py-2 text-sm transition-colors hover:border-primary/35 hover:bg-base-100/70"
									href={`https://musicbrainz.org/release/${edition.release_mbid}`}
									target="_blank"
									rel="noreferrer"
								>
									<span class="min-w-0">
										<span class="block truncate font-medium">{edition.title ?? album.title}</span>
										<span class="block truncate text-xs text-base-content/50">
											{[edition.date?.slice(0, 4), edition.country, edition.packaging]
												.filter(Boolean)
												.join(' · ') || `${edition.track_count} tracks`}
										</span>
									</span>
									<ExternalLink class="h-3.5 w-3.5 shrink-0 text-base-content/35" />
								</a>
							</li>
						{/each}
					</ul>
				{/if}
			{:else}
				<p class="mt-2 text-sm text-base-content/60">
					Link a MusicBrainz release group to compare editions.
				</p>
			{/if}
		</section>
	{/if}
</main>
