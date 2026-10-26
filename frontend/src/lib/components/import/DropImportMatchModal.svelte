<script lang="ts">
	import { untrack } from 'svelte';
	import { Search } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { matchHeldMutation } from '$lib/queries/downloads/HeldQueries.svelte';
	import { getAlbumSearchQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import type { Album } from '$lib/types';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	// Curators pick the album a held entry's files belong to; the app pairs them.
	interface Props {
		entryId: number;
		/** The upload's or album's name, shown under the title. */
		name?: string;
		/** What the search starts with. */
		search: string;
		fileCount: number;
		/** The album the entry is held under, refreshed after the match. */
		releaseGroupMbid?: string | null;
		onclose: () => void;
	}
	let { entryId, name, search, fileCount, releaseGroupMbid = null, onclose }: Props = $props();

	let open = $state(true);
	// initial value only, by design - the modal is keyed to one entry
	let searchTerm = $state(untrack(() => search.replace(/[-_]/g, ' ').trim()));

	const searchQuery = getAlbumSearchQuery(() => searchTerm);
	const match = matchHeldMutation();
	const results = $derived(searchQuery.data ?? []);
	const label = $derived(`${name || search} · ${fileCount} file${fileCount === 1 ? '' : 's'}`);

	async function pick(album: Album) {
		if (match.isPending) return;
		try {
			await match.mutateAsync({
				entryId,
				matchMbid: album.musicbrainz_id,
				releaseGroupMbid
			});
			onclose();
		} catch {
			// the mutation's onError already toasted; keep the modal open to retry
		}
	}
</script>

<Modal
	bind:open
	title="Match to an album"
	description={label}
	size="lg"
	closeButton
	dismissible={!match.isPending}
	{onclose}
>
	<div class="relative">
		<Input
			label="Search for an album"
			icon={Search}
			placeholder="Search for the album…"
			class="w-full"
			bind:value={searchTerm}
		/>
		{#if searchQuery.isFetching}
			<span
				class="loading loading-spinner loading-xs absolute top-1/2 right-3 -translate-y-1/2"
				aria-hidden="true"
			></span>
		{/if}
	</div>

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
</Modal>
