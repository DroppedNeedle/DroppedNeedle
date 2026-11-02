<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { createUnhideDiscoveryV3Mutation } from '$lib/queries/discover/DiscoverV3Mutations.svelte';
	import { getIgnoredReleasesV3Query } from '$lib/queries/discover/DiscoverV3Queries.svelte';
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
			<p class="py-6 text-center text-sm text-base-content/70">Nothing hidden.</p>
		{/snippet}
		{#snippet children(data)}
			<ul class="divide-y divide-base-content/5">
				{#each data.items as album (album.release_group_mbid)}
					<li class="flex items-center gap-3 py-2">
						<AlbumImage
							mbid={album.release_group_mbid}
							alt={album.release_name}
							size="sm"
							className="shrink-0"
						/>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-semibold">{album.release_name}</p>
							<p class="truncate text-xs text-base-content/70">{album.artist_name}</p>
						</div>
						<AsyncButton
							variant="ghost"
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
