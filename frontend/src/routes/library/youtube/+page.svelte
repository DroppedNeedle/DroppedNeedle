<script lang="ts">
	import { onMount } from 'svelte';
	import { YOUTUBE_ENDPOINTS } from '$lib/queries/youtube/endpoints';
	import { integrationStore } from '$lib/stores/integration';
	import { api } from '$lib/api/client';
	import { withBasePath } from '$lib/utils/basePath';
	import { isAbortError } from '$lib/utils/errorHandling';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import YouTubeLinkModal from '$lib/components/YouTubeLinkModal.svelte';
	import YouTubeDetailModal from '$lib/components/YouTubeDetailModal.svelte';
	import ContextMenu from '$lib/components/ContextMenu.svelte';
	import type { MenuItem } from '$lib/components/ContextMenu.svelte';
	import type { YouTubeLink } from '$lib/types';
	import { Plus, Info, Play, Shuffle, ListPlus, ListStart, ListMusic } from 'lucide-svelte';
	import LibraryFilterBar from '$lib/components/LibraryFilterBar.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import {
		ytCardQuickPlay,
		ytCardQuickShuffle,
		ytCardAddToQueue,
		ytCardPlayNext,
		ytCardAddToPlaylist
	} from '$lib/utils/youtubeCardPlayback';

	let links = $state<YouTubeLink[]>([]);
	let loading = $state(true);
	let searchQuery = $state('');
	let playingAlbumId = $state<string | null>(null);
	let shufflingAlbumId = $state<string | null>(null);

	let filteredLinks = $derived(
		searchQuery.trim()
			? links.filter((l) => {
					const q = searchQuery.trim().toLowerCase();
					return l.album_name.toLowerCase().includes(q) || l.artist_name.toLowerCase().includes(q);
				})
			: links
	);

	let editModalOpen = $state(false);
	let editingLink = $state<YouTubeLink | null>(null);

	let detailModalOpen = $state(false);
	let detailLink = $state<YouTubeLink | null>(null);
	let returnToDetailAfterEdit = $state<YouTubeLink | null>(null);

	async function fetchLinks(): Promise<void> {
		loading = true;
		try {
			links = await api.v3.GET(YOUTUBE_ENDPOINTS.links());
		} catch (e) {
			if (isAbortError(e)) return;
		} finally {
			loading = false;
		}
	}

	function openAddModal(): void {
		editingLink = null;
		editModalOpen = true;
	}

	function openDetail(link: YouTubeLink): void {
		detailLink = link;
		detailModalOpen = true;
	}

	function handleDetailEdit(link: YouTubeLink): void {
		returnToDetailAfterEdit = link;
		detailModalOpen = false;
		editingLink = link;
		editModalOpen = true;
	}

	function handleDetailDelete(albumId: string): void {
		links = links.filter((l) => l.album_id !== albumId);
		detailLink = null;
	}

	function handleDetailClose(): void {
		detailLink = null;
	}

	function handleEditModalSave(link: YouTubeLink): void {
		if (editingLink) {
			links = links.map((l) => (l.album_id === link.album_id ? link : l));
		} else {
			links = [link, ...links];
		}
		if (returnToDetailAfterEdit) {
			const updated = links.find((l) => l.album_id === link.album_id);
			if (updated) {
				detailLink = updated;
				detailModalOpen = true;
			}
			returnToDetailAfterEdit = null;
		}
		editingLink = null;
	}

	function handleEditModalClose(): void {
		if (returnToDetailAfterEdit) {
			const current = links.find((l) => l.album_id === returnToDetailAfterEdit!.album_id);
			if (current) {
				detailLink = current;
				detailModalOpen = true;
			}
			returnToDetailAfterEdit = null;
		}
		editingLink = null;
	}

	function getCardMenuItems(link: YouTubeLink): MenuItem[] {
		return [
			{
				label: 'Add to Queue',
				icon: ListPlus,
				onclick: () => void ytCardAddToQueue(link)
			},
			{
				label: 'Play Next',
				icon: ListStart,
				onclick: () => void ytCardPlayNext(link)
			},
			{
				label: 'Add to Playlist',
				icon: ListMusic,
				onclick: () => void ytCardAddToPlaylist(link)
			}
		];
	}

	async function quickPlay(link: YouTubeLink, e: Event): Promise<void> {
		e.stopPropagation();
		e.preventDefault();
		if (playingAlbumId === link.album_id) return;
		playingAlbumId = link.album_id;
		try {
			await ytCardQuickPlay(link);
		} finally {
			playingAlbumId = null;
		}
	}

	async function quickShuffle(link: YouTubeLink, e: Event): Promise<void> {
		e.stopPropagation();
		e.preventDefault();
		if (shufflingAlbumId === link.album_id) return;
		shufflingAlbumId = link.album_id;
		try {
			await ytCardQuickShuffle(link);
		} finally {
			shufflingAlbumId = null;
		}
	}

	onMount(() => {
		fetchLinks();
	});
</script>

<div class="container mx-auto p-6">
	<div class="flex items-center gap-3 mb-6">
		<YouTubeIcon class="h-8 w-8 text-red-500" />
		<h1 class="text-2xl font-bold">YouTube Links</h1>
		<span class="badge badge-neutral">{links.length}</span>
	</div>

	{#if !$integrationStore.youtube}
		<div class="alert alert-warning mb-4">
			<Info class="h-4 w-4" />
			<span
				>YouTube is not enabled. <a href={withBasePath('/settings?tab=youtube')} class="link"
					>Enable it in settings</a
				> to use YouTube features.</span
			>
		</div>
	{:else if !$integrationStore.youtube_api}
		<div class="alert alert-info mb-4">
			<Info class="h-4 w-4" />
			<span
				>YouTube API is not configured. You can add links manually, or <a
					href={withBasePath('/settings?tab=youtube')}
					class="link">enable the API in settings</a
				> for auto-generation.</span
			>
		</div>
	{/if}

	{#if !loading && links.length > 0}
		<LibraryFilterBar
			bind:searchQuery
			placeholder="Search links..."
			ariaLabel="Search YouTube links"
			resultCount={searchQuery.trim() ? filteredLinks.length : null}
		/>
	{/if}

	{#if loading}
		<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4">
			{#each Array(12) as _, i (`skeleton-${i}`)}
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
			<button
				class="flex aspect-square w-full cursor-pointer items-center justify-center rounded-box border-2 border-dashed border-base-content/20 bg-base-100 shadow-sm transition-colors hover:border-accent"
				onclick={openAddModal}
			>
				<div class="flex flex-col items-center gap-2 opacity-60">
					<Plus class="h-10 w-10" />
					<span class="text-sm font-medium">Add Link</span>
				</div>
			</button>

			{#each filteredLinks as link (link.album_id)}
				<Card
					variant="raised"
					padding="none"
					class="group relative w-full cursor-pointer transition hover:shadow-md focus-visible:outline-2 focus-visible:outline-primary motion-safe:hover:-translate-y-0.5"
					onclick={() => openDetail(link)}
					onkeydown={(e) => {
						if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) {
							e.preventDefault();
							openDetail(link);
						}
					}}
					role="button"
					tabindex={0}
					aria-label={`Open ${link.album_name}`}
				>
					<figure class="aspect-square overflow-hidden relative">
						<AlbumImage
							mbid={link.album_id}
							customUrl={link.cover_url}
							alt={link.album_name}
							size="full"
							requestSize={250}
							rounded="none"
							className="w-full h-full"
						/>
						<div class="absolute top-2 left-2">
							<div class="badge badge-sm badge-error gap-1">
								<YouTubeIcon class="h-3 w-3" />
							</div>
						</div>
						<div class="absolute top-2 right-2 flex items-start gap-1">
							{#if link.video_id}
								<div class="badge badge-sm badge-info">Full Video</div>
							{/if}
							{#if link.track_count > 0}
								<div class="badge badge-sm badge-accent">{link.track_count} tracks</div>
							{/if}
						</div>
						<div class="absolute top-1 right-1 z-30">
							<div
								class="rounded-full bg-black/50 backdrop-blur-sm opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 transition-opacity"
							>
								<ContextMenu items={getCardMenuItems(link)} position="end" size="sm" />
							</div>
						</div>

						<div
							class="absolute bottom-2 right-2 flex items-center gap-1.5 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 transition-opacity z-20"
						>
							<IconButton
								variant="neutral"
								size="sm"
								icon={Shuffle}
								label="Shuffle {link.album_name}"
								loading={shufflingAlbumId === link.album_id}
								onclick={(e) => quickShuffle(link, e)}
							/>
							<IconButton
								variant="primary"
								size="sm"
								icon={Play}
								label="Play {link.album_name}"
								class="[&_svg]:fill-current"
								loading={playingAlbumId === link.album_id}
								onclick={(e) => quickPlay(link, e)}
							/>
						</div>
					</figure>

					<div class="p-3">
						<CardTitle class="text-sm line-clamp-2 min-h-10">{link.album_name}</CardTitle>
						<p class="text-xs opacity-70 line-clamp-1">{link.artist_name}</p>
					</div>
				</Card>
			{/each}
		</div>

		{#if filteredLinks.length === 0 && links.length > 0}
			<Card class="mt-4 text-center">
				<p class="text-lg opacity-60">No matching links</p>
				<p class="text-sm opacity-40">Try a different search term.</p>
			</Card>
		{:else if links.length === 0}
			<Card class="mt-4 text-center">
				<YouTubeIcon class="mx-auto h-12 w-12 opacity-20" />
				<p class="text-lg opacity-60">No saved YouTube links</p>
				<p class="text-sm opacity-40">Generate links from album pages or add them manually.</p>
			</Card>
		{/if}
	{/if}
</div>

<YouTubeDetailModal
	bind:open={detailModalOpen}
	link={detailLink}
	onclose={handleDetailClose}
	onedit={handleDetailEdit}
	ondelete={handleDetailDelete}
/>

<YouTubeLinkModal
	bind:open={editModalOpen}
	editLink={editingLink}
	onclose={handleEditModalClose}
	onsave={handleEditModalSave}
/>
