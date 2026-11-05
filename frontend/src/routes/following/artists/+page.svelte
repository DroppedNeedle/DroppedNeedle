<script lang="ts">
	import { Heart, X } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import FollowEmpty from '$lib/features/following/components/FollowEmpty.svelte';
	import { getFollowedArtistsQuery } from '$lib/features/following/queries/FollowQueries.svelte';
	import { createUnfollowMutation } from '$lib/features/following/queries/FollowMutations.svelte';
	import type { FollowedArtist } from '$lib/features/following/queries/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { artistHref } from '$lib/utils/entityRoutes';

	const query = getFollowedArtistsQuery();
	const artists = $derived(query.data ?? []);
	const unfollow = createUnfollowMutation();

	function stateChip(
		a: FollowedArtist
	): { label: string; tone: 'success' | 'warning' | 'neutral' } | null {
		if (!a.auto_download) return null;
		if (a.auto_download_state === 'approved') return { label: 'Auto-download', tone: 'success' };
		if (a.auto_download_state === 'pending') return { label: 'Pending', tone: 'warning' };
		return { label: 'Auto-download', tone: 'neutral' };
	}
</script>

<svelte:head>
	<title>Your Artists - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader
		title="Your Artists"
		ticket="DN / CC 3.2 - ART"
		backHref={withBasePath('/following')}
		backLabel="Back to Following"
	/>

	<div class="px-4 pb-10 sm:px-6 lg:px-8">
		{#if query.isPending}
			<div class="dn-list-grid">
				{#each Array(10) as _, i (i)}
					<MediaTileSkeleton kind="artist" />
				{/each}
			</div>
		{:else if artists.length === 0}
			<div class="fa-empty">
				<FollowEmpty
					icon={Heart}
					titleAs="h2"
					title="You are not following anyone yet"
					description="Follow an artist from their page to keep up with their new releases here."
					ctaLabel="Find artists"
					ctaHref={withBasePath('/search')}
				/>
			</div>
		{:else}
			<div class="dn-list-grid">
				{#each artists as a (a.mbid)}
					{@const chip = stateChip(a)}
					<MediaTile
						kind="artist"
						title={a.name}
						titleAs="h2"
						href={artistHref(a.mbid)}
						label="Open {a.name}"
					>
						{#snippet art()}
							<ArtistImage
								mbid={a.mbid}
								alt={a.name}
								size="full"
								requestSize={250}
								className="h-full w-full"
							/>
						{/snippet}
						{#snippet footer()}
							{#if chip}
								<StatusChip size="xs" tone={chip.tone}>{chip.label}</StatusChip>
							{/if}
						{/snippet}
						{#snippet overlay()}
							<IconButton
								variant="danger"
								size="xs"
								icon={X}
								label="Unfollow {a.name}"
								disabled={unfollow.isPending}
								class="absolute top-1 right-1 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
								onclick={() => unfollow.mutate(a.mbid)}
							/>
						{/snippet}
					</MediaTile>
				{/each}
			</div>
		{/if}
	</div>
</div>

<style>
	.fa-empty {
		max-width: 46rem;
		margin-inline: auto;
		padding-top: 0.5rem;
	}
</style>
