<script lang="ts">
	import { TOAST_DURATION } from '$lib/constants';
	import {
		createManualYouTubeLinkMutation,
		createUpdateYouTubeLinkMutation
	} from '$lib/queries/youtube/YouTubeQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getCoverUrl } from '$lib/utils/errorHandling';
	import { getSearchBucketV3Query } from '$lib/queries/search/SearchV3Queries.svelte';
	import { toMusicBrainzAlbums } from '$lib/queries/search/SearchV3Adapters';
	import type { YouTubeLink, Album } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import TextField from '$lib/ui/TextField.svelte';

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

	const saveManual = createManualYouTubeLinkMutation();
	const updateLink = createUpdateYouTubeLinkMutation();

	let searchQuery = $state('');
	// The term the search runs for, set once typing pauses.
	let searchTerm = $state('');
	let typing = $state(false);
	let searchTimeout: ReturnType<typeof setTimeout> | null = null;
	const albumSearch = getSearchBucketV3Query(
		() => 'albums',
		() => searchTerm,
		() => 50
	);
	const searchResults = $derived<Album[]>(
		searchTerm && albumSearch.data ? toMusicBrainzAlbums(albumSearch.data.results) : []
	);
	const searching = $derived(typing || (searchTerm !== '' && albumSearch.isFetching));

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
		searchTerm = '';
		mode = 'manual';
	}

	function handleSearchInput(): void {
		if (searchTimeout) clearTimeout(searchTimeout);
		const term = searchQuery.trim();
		if (term.length < 2) {
			searchTerm = '';
			typing = false;
			return;
		}
		typing = true;
		searchTimeout = setTimeout(() => {
			searchTerm = term;
			typing = false;
		}, 400);
	}

	function selectAlbum(album: Album): void {
		albumName = album.title;
		artistName = album.artist || '';
		coverUrl = getCoverUrl(album.cover_url, album.musicbrainz_id);
		selectedAlbumId = album.musicbrainz_id;
		searchTerm = '';
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
				const updated = await updateLink.mutateAsync({
					albumId: selectedAlbumId,
					body: {
						youtube_url: youtubeUrl.trim(),
						album_name: albumName.trim(),
						artist_name: artistName.trim(),
						cover_url: normalizedCoverUrl
					}
				});
				onsave(updated);
				toastStore.show({ message: 'Link updated', type: 'success', duration: TOAST_DURATION });
			} else {
				const link = await saveManual.mutateAsync({
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

<Modal bind:open title={isEditing ? 'Edit YouTube Link' : 'Add YouTube Link'} onclose={handleClose}>
	{#if !isEditing}
		<div role="tablist" class="tabs tabs-box mb-4">
			<button
				type="button"
				role="tab"
				class="tab"
				class:tab-active={mode === 'manual'}
				aria-selected={mode === 'manual'}
				onclick={() => {
					mode = 'manual';
				}}>Manual Entry</button
			>
			<button
				type="button"
				role="tab"
				class="tab"
				class:tab-active={mode === 'search'}
				aria-selected={mode === 'search'}
				onclick={() => {
					mode = 'search';
				}}>Search Album</button
			>
		</div>
	{/if}

	{#if mode === 'search' && !isEditing}
		<div class="mb-4">
			<Field label="Search for an album">
				{#snippet children(control)}
					<Input
						{...control}
						label="Search for an album"
						class="w-full"
						placeholder="Search albums..."
						bind:value={searchQuery}
						oninput={handleSearchInput}
					/>
				{/snippet}
			</Field>
			{#if searching}
				<div class="flex justify-center py-4">
					<span class="loading loading-spinner loading-sm"></span>
				</div>
			{:else if searchResults.length > 0}
				<div class="mt-2 max-h-48 overflow-y-auto rounded-box bg-base-200">
					{#each searchResults as album (album.musicbrainz_id)}
						<button
							type="button"
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
			<TextField label="Album Name" placeholder="Album name" required bind:value={albumName} />
			<TextField label="Artist" placeholder="Artist name" required bind:value={artistName} />
			<TextField
				label="YouTube URL"
				type="url"
				placeholder="https://www.youtube.com/watch?v=..."
				required
				bind:value={youtubeUrl}
			/>
			<TextField
				label="Cover Image URL"
				type="url"
				placeholder="https://example.com/cover.jpg (optional)"
				bind:value={coverUrl}
			/>

			{#if selectedAlbumId}
				<div class="text-xs opacity-50">
					Linked to album: <span class="font-mono">{selectedAlbumId}</span>
				</div>
			{/if}
		</div>
	{/if}

	{#snippet actions()}
		<Button variant="ghost" onclick={handleClose}>Cancel</Button>
		<Button variant="accent" loading={saving} disabled={!isValid} onclick={handleSave}>
			{isEditing ? 'Update' : 'Add Link'}
		</Button>
	{/snippet}
</Modal>
