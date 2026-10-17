<script lang="ts">
	import { Heart, X, ArrowLeft } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { getFollowedArtistsQuery } from '$lib/queries/following/FollowQueries.svelte';
	import { createUnfollowMutation } from '$lib/queries/following/FollowMutations.svelte';
	import type { FollowedArtist } from '$lib/queries/following/types';
	import { withBasePath } from '$lib/utils/basePath';
	import IconButton from '$lib/ui/IconButton.svelte';

	const query = getFollowedArtistsQuery();
	const artists = $derived(query.data ?? []);
	const unfollow = createUnfollowMutation();

	function stateChip(a: FollowedArtist): { label: string; cls: string } | null {
		if (!a.auto_download) return null;
		if (a.auto_download_state === 'approved')
			return { label: 'Auto-download', cls: 'badge-success' };
		if (a.auto_download_state === 'pending') return { label: 'Pending', cls: 'badge-warning' };
		return { label: 'Auto-download', cls: 'badge-ghost' };
	}
</script>

<svelte:head>
	<title>Your Artists - DroppedNeedle</title>
</svelte:head>

<div class="mx-auto w-full max-w-6xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
	<div class="mb-6 flex items-center gap-3">
		<IconButton
			size="sm"
			icon={ArrowLeft}
			label="Back to Following"
			href={withBasePath('/following')}
		/>
		<Heart class="h-6 w-6 text-primary" aria-hidden="true" />
		<h1 class="text-2xl font-bold sm:text-3xl">Your Artists</h1>
		<div class="flex-1"></div>
	</div>

	{#if query.isPending}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
			{#each Array(10) as _, i (i)}
				<div class="aspect-square w-full animate-pulse rounded-2xl bg-base-200"></div>
			{/each}
		</div>
	{:else if artists.length === 0}
		<EmptyState
			icon={Heart}
			title="You are not following anyone yet"
			description="Follow an artist from their page to keep up with their new releases here."
			ctaLabel="Find artists"
			ctaHref={withBasePath('/search')}
		/>
	{:else}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
			{#each artists as a (a.mbid)}
				{@const chip = stateChip(a)}
				<div class="group flex flex-col gap-2">
					<div class="relative overflow-hidden rounded-2xl">
						<a href={withBasePath(`/artist/${a.mbid}`)} aria-label="Open {a.name}">
							<ArtistImage
								mbid={a.mbid}
								alt={a.name}
								className="w-full aspect-square object-cover transition-transform duration-300 group-hover:scale-105"
							/>
						</a>
						<IconButton
							variant="danger"
							size="xs"
							icon={X}
							label="Unfollow {a.name}"
							disabled={unfollow.isPending}
							class="absolute right-2 top-2 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
							onclick={() => unfollow.mutate(a.mbid)}
						/>
					</div>
					<a href={withBasePath(`/artist/${a.mbid}`)} class="truncate font-semibold hover:underline"
						>{a.name}</a
					>
					{#if chip}
						<span class="badge badge-sm {chip.cls} w-fit">{chip.label}</span>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>
