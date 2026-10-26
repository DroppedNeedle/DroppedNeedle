<script lang="ts" generics="TAlbum extends RemoteAlbum | LocalAlbumSummary">
	import type { Snippet } from 'svelte';
	import type { LibraryController } from '$lib/utils/libraryController.svelte';
	import type { LocalAlbumSummary } from '$lib/types';
	import type { RemoteAlbum } from '$lib/queries/remotes/types';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import AddToPlaylistModal from '$lib/components/AddToPlaylistModal.svelte';
	import SourceAlbumModal from '$lib/components/SourceAlbumModal.svelte';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { ChevronLeft, CircleX, Play, Shuffle } from 'lucide-svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { onMount, onDestroy } from 'svelte';

	interface Props {
		ctrl: LibraryController<TAlbum>;
		headerIcon: Snippet;
		headerTitle: string;
		recentLabel?: string;
		moodLabel?: string;
		statsPanel?: Snippet;
		recentCardOverlay?: Snippet<[TAlbum]>;
		cardTopLeftBadge: Snippet<[TAlbum]>;
		cardTopRightExtra?: Snippet<[TAlbum]>;
		cardBottomLeft?: Snippet<[TAlbum]>;
		cardBodyExtra?: Snippet<[TAlbum]>;
		backHref?: string;
		contextMenuBackdrop?: boolean;
		decades?: string[];
		tags?: string[];
		emptyIcon: Snippet;
		emptyTitle: string;
		emptyDescription: string;
	}

	let {
		ctrl,
		headerIcon,
		headerTitle,
		recentLabel = 'Recently played',
		moodLabel = 'Mood',
		statsPanel,
		recentCardOverlay,
		cardTopLeftBadge,
		cardTopRightExtra,
		cardBottomLeft,
		cardBodyExtra,
		backHref,
		contextMenuBackdrop = false,
		decades,
		tags,
		emptyIcon,
		emptyTitle,
		emptyDescription
	}: Props = $props();

	const a = $derived(ctrl.adapter);

	onMount(() => ctrl.init());
	onDestroy(() => ctrl.cleanup());
</script>

<div class="container mx-auto p-6">
	<div class="flex items-center gap-3 mb-2">
		{#if backHref}
			<IconButton size="sm" href={withBasePath(backHref)} icon={ChevronLeft} label="Back" />
		{/if}
		{@render headerIcon()}
		<h1 class="text-2xl font-bold">{headerTitle}</h1>
		{#if ctrl.stats}
			<span class="badge badge-neutral"
				>{(ctrl.stats as { total_albums?: number }).total_albums ?? 0} albums</span
			>
		{/if}
	</div>

	{#if statsPanel}
		{@render statsPanel()}
	{/if}

	{#if ctrl.recentAlbums.length > 0}
		<div class="mb-8">
			<h2 class="text-lg font-semibold mb-3 opacity-80">{recentLabel}</h2>
			<div class="flex gap-3 overflow-x-auto pb-2">
				{#each ctrl.recentAlbums as album (a.getAlbumId(album))}
					<button
						class="shrink-0 w-36 group cursor-pointer transition-transform hover:scale-105 active:scale-95"
						onclick={() => ctrl.openDetail(album)}
					>
						<div class="aspect-square rounded-lg overflow-hidden shadow-sm relative">
							<AlbumImage
								mbid={a.getAlbumMbid(album) ?? String(a.getAlbumId(album))}
								customUrl={a.getAlbumImageUrl(album)}
								alt={a.getAlbumName(album)}
								size="full"
								requestSize={250}
								rounded="none"
								className="w-full h-full"
							/>
							{#if recentCardOverlay}
								{@render recentCardOverlay(album)}
							{/if}
						</div>
						<p class="text-sm font-medium mt-1 line-clamp-1">{a.getAlbumName(album)}</p>
						<p class="text-xs opacity-60 line-clamp-1">{a.getArtistName(album)}</p>
					</button>
				{/each}
			</div>
		</div>
	{/if}

	{#if a.supportsFavorites && ctrl.favoriteAlbums.length > 0}
		<div class="mb-8">
			<h2 class="text-lg font-semibold mb-3 opacity-80">Favorites</h2>
			<div class="flex gap-3 overflow-x-auto pb-2">
				{#each ctrl.favoriteAlbums as album (a.getAlbumId(album))}
					<button
						class="shrink-0 w-36 group cursor-pointer transition-transform hover:scale-105 active:scale-95"
						onclick={() => ctrl.openDetail(album)}
					>
						<div class="aspect-square rounded-lg overflow-hidden shadow-sm">
							<AlbumImage
								mbid={a.getAlbumMbid(album) ?? String(a.getAlbumId(album))}
								customUrl={a.getAlbumImageUrl(album)}
								alt={a.getAlbumName(album)}
								size="full"
								requestSize={250}
								rounded="none"
								className="w-full h-full"
							/>
						</div>
						<p class="text-sm font-medium mt-1 line-clamp-1">{a.getAlbumName(album)}</p>
						<p class="text-xs opacity-60 line-clamp-1">{a.getArtistName(album)}</p>
					</button>
				{/each}
			</div>
		</div>
	{/if}

	<div class="mb-6">
		<h2 class="text-lg font-semibold mb-3 opacity-80">All albums</h2>
		<LibraryFilterBar
			bind:searchQuery={ctrl.searchQuery}
			onSearchInput={ctrl.handleSearch}
			sortOptions={a.sortOptions}
			sortBy={ctrl.sortBy}
			onSortChange={ctrl.handleSortChange}
			sortOrder={ctrl.sortOrder}
			onToggleSortOrder={ctrl.toggleSortOrder}
			ascValue={a.ascValue}
			genres={a.supportsGenres ? ctrl.genres : undefined}
			selectedGenre={a.supportsGenres ? ctrl.selectedGenre : undefined}
			onGenreChange={a.supportsGenres ? ctrl.handleGenreChange : undefined}
			moods={a.supportsMoods ? ctrl.moods : undefined}
			selectedMood={a.supportsMoods ? ctrl.selectedMood : undefined}
			onMoodChange={a.supportsMoods ? ctrl.handleMoodChange : undefined}
			{moodLabel}
			decades={a.supportsDecades ? decades : undefined}
			selectedDecade={a.supportsDecades ? ctrl.selectedDecade : undefined}
			onDecadeChange={a.supportsDecades ? ctrl.handleDecadeChange : undefined}
			tags={a.supportsTags ? tags : undefined}
			selectedTag={a.supportsTags ? ctrl.selectedTag : undefined}
			onTagChange={a.supportsTags ? ctrl.handleTagChange : undefined}
			resultCount={ctrl.loading ? null : ctrl.total}
			loading={ctrl.loading}
		/>
	</div>

	{#if ctrl.fetchError}
		<div role="alert" class="alert alert-error alert-soft mb-4">
			<CircleX class="h-6 w-6 shrink-0" />
			<div class="flex flex-col gap-1">
				<span>{ctrl.fetchError}</span>
				{#if ctrl.fetchErrorCode === 'CIRCUIT_BREAKER_OPEN' || /connection|DNS|not configured/i.test(ctrl.fetchError)}
					<a href={withBasePath('/settings')} class="link link-primary text-sm">Open settings</a>
				{/if}
			</div>
			<Button variant="ghost" size="sm" onclick={() => ctrl.fetchAlbums(true)}>Retry</Button>
		</div>
	{/if}

	{#if ctrl.loading}
		<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4">
			{#each Array(12) as _, i (i)}
				<Card variant="raised" padding="none" class="animate-pulse">
					<div class="aspect-square bg-base-300"></div>
					<div class="p-3">
						<div class="h-4 bg-base-300 rounded w-3/4"></div>
						<div class="h-3 bg-base-300 rounded w-1/2 mt-1"></div>
					</div>
				</Card>
			{/each}
		</div>
	{:else}
		<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4">
			{#each ctrl.albums as album (a.getAlbumId(album))}
				<Card
					variant="raised"
					padding="none"
					class="group relative w-full cursor-pointer transition hover:shadow-md focus-visible:outline-2 focus-visible:outline-primary motion-safe:hover:-translate-y-0.5"
					onclick={() => ctrl.openDetail(album)}
					onkeydown={(e) => {
						if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) {
							e.preventDefault();
							ctrl.openDetail(album);
						}
					}}
					role="button"
					tabindex={0}
					aria-label={`Open ${a.getAlbumName(album)}`}
				>
					<figure class="aspect-square overflow-hidden relative">
						<AlbumImage
							mbid={a.getAlbumMbid(album) ?? String(a.getAlbumId(album))}
							customUrl={a.getAlbumImageUrl(album)}
							alt={a.getAlbumName(album)}
							size="full"
							requestSize={250}
							rounded="none"
							className="w-full h-full"
						/>
						<div class="absolute top-2 left-2 flex items-center gap-1">
							{@render cardTopLeftBadge(album)}
						</div>
						<div class="absolute top-2 right-2 flex items-start gap-1">
							{#if cardTopRightExtra}
								{@render cardTopRightExtra(album)}
							{:else if a.getAlbumYear(album)}
								<div class="badge badge-sm badge-ghost">{a.getAlbumYear(album)}</div>
							{/if}
							{#if contextMenuBackdrop}
								<div class="rounded-full bg-black/50 backdrop-blur-sm">
									<ContextMenu items={ctrl.getAlbumMenuItems(album)} position="end" size="xs" />
								</div>
							{:else}
								<div>
									<ContextMenu items={ctrl.getAlbumMenuItems(album)} position="end" size="xs" />
								</div>
							{/if}
						</div>
						{#if cardBottomLeft}
							<div class="absolute bottom-2 left-2">
								{@render cardBottomLeft(album)}
							</div>
						{/if}
						<div
							class="absolute bottom-2 right-2 flex items-center gap-1.5 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 transition-opacity"
						>
							{#if a.supportsShuffle}
								<IconButton
									variant="neutral"
									size="sm"
									icon={Shuffle}
									label="Shuffle {a.getAlbumName(album)}"
									onclick={(e) => ctrl.quickShuffle(album, e)}
								/>
							{/if}
							<IconButton
								variant="primary"
								size="sm"
								icon={Play}
								label="Play {a.getAlbumName(album)}"
								class="[&_svg]:fill-current"
								loading={ctrl.playingAlbumId === a.getAlbumId(album)}
								onclick={(e) => ctrl.quickPlay(album, e)}
							/>
						</div>
					</figure>

					<div class="flex flex-col gap-2 p-3">
						<CardTitle class="text-sm line-clamp-2 min-h-10">
							{a.getAlbumName(album)}
						</CardTitle>
						{#if cardBodyExtra}
							{@render cardBodyExtra(album)}
						{:else}
							<p class="text-xs opacity-70 line-clamp-1">{a.getArtistName(album)}</p>
						{/if}
					</div>
				</Card>
			{/each}
		</div>

		{#if ctrl.albums.length === 0}
			<Card class="mt-4 text-center">
				<div class="flex justify-center">{@render emptyIcon()}</div>
				<p class="text-lg opacity-60">{emptyTitle}</p>
				<p class="text-sm opacity-40">{emptyDescription}</p>
			</Card>
		{/if}

		{#if ctrl.albums.length < ctrl.total}
			<div class="flex justify-center mt-6">
				<Button variant="ghost" loading={ctrl.loadingMore} onclick={ctrl.loadMore}>Show more</Button
				>
			</div>
		{/if}
	{/if}
</div>

<AddToPlaylistModal bind:this={ctrl.playlistModalRef} />

<SourceAlbumModal
	bind:open={ctrl.detailModalOpen}
	sourceType={a.sourceType}
	album={ctrl.selectedAlbum}
	onclose={ctrl.handleDetailClose}
/>
