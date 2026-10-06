<script lang="ts">
	import { API, TOAST_DURATION } from '$lib/constants';
	import { toastStore } from '$lib/stores/toast';
	import YouTubeIcon from '$lib/components/YouTubeIcon.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { api } from '$lib/api/client';
	import type { YouTubeLink, Album } from '$lib/types';
	import { X } from 'lucide-svelte';

	interface Props {
		open: boolean;
		editLink?: YouTubeLink | null;
		onclose: () => void;
		onsave: (link: YouTubeLink) => void;
	}

	let { open = $bindable(), editLink = null, onclose, onsave }: Props = $props();

	let mode = $state<'search' | 'manual'>('manual');
	let albumName = $state('');
	let artistName = $state('');
	let youtubeUrl = $state('');
	let coverUrl = $state('');
	let selectedAlbumId = $state<string | null>(null);
	let saving = $state(false);

	let searchQuery = $state('');
	let searchResults = $state<Album[]>([]);
	let searching = $state(false);
	let searchTimeout: ReturnType<typeof setTimeout> | null = null;

	const isEditing = $derived(editLink !== null);
	const isValid = $derived(
		albumName.trim() !== '' && artistName.trim() !== '' && youtubeUrl.trim() !== ''
	);

	$effect(() => {
		if (open && editLink) {
			albumName = editLink.album_name;
			artistName = editLink.artist_name;
			youtubeUrl = editLink.video_id ? `https://www.youtube.com/watch?v=${editLink.video_id}` : '';
			coverUrl = getCoverUrl(editLink.cover_url, editLink.album_id);
			selectedAlbumId = editLink.album_id;
			mode = 'manual';
		} else if (open && !editLink) {
			resetForm();
		}
	});

	function resetForm(): void {
		albumName = '';
		artistName = '';
		youtubeUrl = '';
		coverUrl = '';
		selectedAlbumId = null;
		searchQuery = '';
		searchResults = [];
		mode = 'manual';
	}

	function handleSearchInput(): void {
		if (searchTimeout) clearTimeout(searchTimeout);
		if (searchQuery.trim().length < 2) {
			searchResults = [];
			return;
		}
		searching = true;
		searchTimeout = setTimeout(async () => {
			try {
				const data = await api.global.get<{ results?: Album[] }>(
					API.search.albums(searchQuery.trim())
				);
				searchResults = data.results ?? [];
			} catch {
				// Ignore errors
			} finally {
				searching = false;
			}
		}, 400);
	}

	function selectAlbum(album: Album): void {
		albumName = album.title;
		artistName = album.artist || '';
		coverUrl = getCoverUrl(album.cover_url, album.musicbrainz_id);
		selectedAlbumId = album.musicbrainz_id;
		searchResults = [];
		searchQuery = '';
		mode = 'manual';
	}

	async function handleSave(): Promise<void> {
		if (!isValid) return;
		saving = true;
		const trimmedCoverUrl = coverUrl.trim() || null;
		const normalizedCoverUrl = selectedAlbumId
			? getCoverUrl(trimmedCoverUrl, selectedAlbumId)
			: trimmedCoverUrl;

		try {
			if (isEditing && selectedAlbumId) {
				const updated = await api.global.put<YouTubeLink>(API.youtube.updateLink(selectedAlbumId), {
					youtube_url: youtubeUrl.trim(),
					album_name: albumName.trim(),
					artist_name: artistName.trim(),
					cover_url: normalizedCoverUrl
				});
				onsave(updated);
				toastStore.show({ message: 'Link updated', type: 'success', duration: TOAST_DURATION });
			} else {
				const link = await api.global.post<YouTubeLink>(API.youtube.manual(), {
					album_name: albumName.trim(),
					artist_name: artistName.trim(),
					youtube_url: youtubeUrl.trim(),
					cover_url: normalizedCoverUrl,
					album_id: selectedAlbumId
				});
				onsave(link);
				toastStore.show({ message: 'Link added', type: 'success', duration: TOAST_DURATION });
			}
			open = false;
			onclose();
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : "Couldn't save the link",
				type: 'error',
				duration: TOAST_DURATION
			});
		} finally {
			saving = false;
		}
	}

	function handleClose(): void {
		if (searchTimeout) clearTimeout(searchTimeout);
		open = false;
		onclose();
	}
</script>

{#if open}
	<dialog class="modal modal-open">
		<div class="modal-box max-w-md">
			<button class="btn btn-sm btn-circle btn-ghost absolute right-2 top-2" onclick={handleClose}
				><X class="h-4 w-4" /></button
			>

			<h3 class="text-lg font-bold flex items-center gap-2 mb-4">
				<YouTubeIcon class="h-5 w-5 text-red-500" />
				{isEditing ? 'Edit YouTube Link' : 'Add YouTube Link'}
			</h3>

			{#if !isEditing}
				<div role="tablist" class="tabs tabs-box mb-4">
					<button
						role="tab"
						class="tab"
						class:tab-active={mode === 'manual'}
						onclick={() => {
							mode = 'manual';
						}}>Manual Entry</button
					>
					<button
						role="tab"
						class="tab"
						class:tab-active={mode === 'search'}
						onclick={() => {
							mode = 'search';
						}}>Search Album</button
					>
				</div>
			{/if}

			{#if mode === 'search' && !isEditing}
				<div class="form-control mb-4">
					<label class="label" for="search-input">
						<span class="label-text">Search for an album</span>
					</label>
					<input
						id="search-input"
						type="text"
						class="input input-bordered w-full"
						placeholder="Search albums..."
						bind:value={searchQuery}
						oninput={handleSearchInput}
					/>
					{#if searching}
						<div class="flex justify-center py-4">
							<span class="loading loading-spinner loading-sm"></span>
						</div>
					{:else if searchResults.length > 0}
						<div class="mt-2 max-h-48 overflow-y-auto rounded-box bg-base-200">
							{#each searchResults as album (album.musicbrainz_id)}
								<button
									class="flex items-center gap-3 w-full p-2 hover:bg-base-300 transition-colors text-left"
									onclick={() => selectAlbum(album)}
								>
									<AlbumImage
										mbid={album.musicbrainz_id}
										customUrl={album.cover_url}
										alt={album.title}
										size="sm"
										rounded="md"
									/>
									<div class="min-w-0 flex-1">
										<p class="text-sm font-medium truncate">{album.title}</p>
										<p class="text-xs opacity-60 truncate">
											{album.artist}{album.year ? ` (${album.year})` : ''}
										</p>
									</div>
								</button>
							{/each}
						</div>
					{/if}
				</div>
			{/if}

			{#if mode === 'manual' || isEditing}
				<div class="space-y-3">
					<div class="form-control">
						<label class="label" for="album-name">
							<span class="label-text">Album Name <span class="text-error">*</span></span>
						</label>
						<input
							id="album-name"
							type="text"
							class="input input-bordered w-full"
							placeholder="Album name"
							bind:value={albumName}
						/>
					</div>

					<div class="form-control">
						<label class="label" for="artist-name">
							<span class="label-text">Artist <span class="text-error">*</span></span>
						</label>
						<input
							id="artist-name"
							type="text"
							class="input input-bordered w-full"
							placeholder="Artist name"
							bind:value={artistName}
						/>
					</div>

					<div class="form-control">
						<label class="label" for="youtube-url">
							<span class="label-text">YouTube URL <span class="text-error">*</span></span>
						</label>
						<input
							id="youtube-url"
							type="url"
							class="input input-bordered w-full"
							placeholder="https://www.youtube.com/watch?v=..."
							bind:value={youtubeUrl}
						/>
					</div>

					<div class="form-control">
						<label class="label" for="cover-url">
							<span class="label-text">Cover Image URL</span>
						</label>
						<input
							id="cover-url"
							type="url"
							class="input input-bordered w-full"
							placeholder="https://example.com/cover.jpg (optional)"
							bind:value={coverUrl}
						/>
					</div>

					{#if selectedAlbumId}
						<div class="text-xs opacity-50">
							Linked to album: <span class="font-mono">{selectedAlbumId}</span>
						</div>
					{/if}
				</div>
			{/if}

			<div class="modal-action">
				<button class="btn btn-ghost" onclick={handleClose}>Cancel</button>
				<button class="btn btn-accent" onclick={handleSave} disabled={!isValid || saving}>
					{#if saving}
						<span class="loading loading-spinner loading-xs"></span>
					{/if}
					{isEditing ? 'Update' : 'Add Link'}
				</button>
			</div>
		</div>
		<form method="dialog" class="modal-backdrop">
			<button onclick={handleClose}>close</button>
		</form>
	</dialog>
{/if}
