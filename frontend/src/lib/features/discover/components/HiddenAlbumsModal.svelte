<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { createUnhideDiscoveryV3Mutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import { getIgnoredReleasesV3Query } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	interface Props {
		open: boolean;
	}

	let { open = $bindable(false) }: Props = $props();

	const ignoredQuery = getIgnoredReleasesV3Query(() => open);
	const unhide = createUnhideDiscoveryV3Mutation();
</script>

<Modal bind:open title="Hidden albums" closeButton>
	<QueryBoundary query={ignoredQuery} what="your hidden albums">
		{#snippet empty()}
			<p class="py-6 text-center text-sm text-muted">Nothing hidden.</p>
		{/snippet}
		{#snippet children(data)}
			<ul class="divide-y divide-line">
				{#each data.items as album (album.release_group_mbid)}
					<li class="flex items-center gap-3 py-2">
						<AlbumImage
							mbid={album.release_group_mbid}
							alt={album.release_name}
							size="sm"
							className="shrink-0"
						/>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-bold">{album.release_name}</p>
							<p class="truncate font-mono text-xs text-muted">{album.artist_name}</p>
						</div>
						<AsyncButton
							variant="outline"
							size="sm"
							aria-label="Un-hide {album.release_name}"
							onclick={() => unhide.mutateAsync(album.release_group_mbid).catch(() => undefined)}
						>
							Un-hide
						</AsyncButton>
					</li>
				{/each}
			</ul>
		{/snippet}
	</QueryBoundary>
</Modal>
