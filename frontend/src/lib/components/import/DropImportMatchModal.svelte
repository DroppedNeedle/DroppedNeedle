<script lang="ts">
	import { untrack } from 'svelte';
	import { Search, X } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getAlbumSearchQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { matchDropItemMutation } from '$lib/queries/import/DropImportMutations.svelte';
	import type { DropImportItem } from '$lib/queries/import/types';
	import type { Album } from '$lib/types';

	interface Props {
		item: DropImportItem;
		onclose: () => void;
	}
	let { item, onclose }: Props = $props();

	let dialogEl = $state<HTMLDialogElement | null>(null);
	// initial value only, by design - the modal is keyed to one item
	let searchTerm = $state(untrack(() => item.folder_name.replace(/[-_]/g, ' ').trim()));

	const searchQuery = getAlbumSearchQuery(() => searchTerm);
	const match = matchDropItemMutation();
	const results = $derived(searchQuery.data ?? []);

	$effect(() => {
		dialogEl?.showModal();
	});

	async function pick(album: Album) {
		if (match.isPending) return;
		try {
			await match.mutateAsync({ itemId: item.id, releaseGroupMbid: album.musicbrainz_id });
			onclose();
		} catch {
			// the mutation's onError already toasted; keep the modal open to retry
		}
	}
</script>

<dialog bind:this={dialogEl} class="modal" {onclose}>
	<div class="modal-box max-w-xl">
		<div class="flex items-start justify-between gap-3">
			<div class="min-w-0">
				<h3 class="text-lg font-bold">Match to an album</h3>
				<p class="truncate text-xs text-base-content/50" title={item.folder_name}>
					{item.folder_name} · {item.files_total} file{item.files_total === 1 ? '' : 's'}
				</p>
			</div>
			<button
				class="btn btn-ghost btn-sm btn-circle"
				onclick={() => dialogEl?.close()}
				aria-label="Close"
			>
				<X class="h-5 w-5" aria-hidden="true" />
			</button>
		</div>

		<label class="input input-bordered mt-4 flex w-full items-center gap-2">
			<Search class="h-4 w-4 opacity-50" aria-hidden="true" />
			<input
				type="text"
				class="grow"
				placeholder="Search for the album…"
				bind:value={searchTerm}
				aria-label="Search for an album"
			/>
			{#if searchQuery.isFetching}<span class="loading loading-spinner loading-xs"></span>{/if}
		</label>

		<div class="mt-3 max-h-72 space-y-1 overflow-y-auto">
			{#each results as album (album.musicbrainz_id)}
				<button
					class="flex w-full items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-base-200 disabled:opacity-50"
					onclick={() => pick(album)}
					disabled={match.isPending}
				>
					<AlbumImage
						mbid={album.musicbrainz_id}
						size="xs"
						rounded="sm"
						className="h-10 w-10 shrink-0"
						alt=""
					/>
					<div class="min-w-0">
						<p class="truncate text-sm font-medium">{album.title}</p>
						<p class="truncate text-xs text-base-content/55">
							{album.artist || 'Unknown artist'}{album.year ? ` · ${album.year}` : ''}
						</p>
					</div>
				</button>
			{:else}
				{#if searchTerm.trim().length >= 2 && !searchQuery.isFetching}
					<p class="p-2 text-sm text-base-content/50">No albums found - try another search.</p>
				{/if}
			{/each}
		</div>

		{#if match.isPending}
			<p class="mt-3 text-sm text-base-content/60">
				<span class="loading loading-spinner loading-xs"></span>
				Importing your files…
			</p>
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop"><button aria-label="Close">close</button></form>
</dialog>
