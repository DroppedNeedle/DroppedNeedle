<script lang="ts">
	import { Heart, X, ArrowLeft, DownloadCloud } from 'lucide-svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import LidarrImportModal from '$lib/components/following/LidarrImportModal.svelte';
	import { getFollowedArtistsQuery } from '$lib/queries/following/FollowQueries.svelte';
	import { createUnfollowMutation } from '$lib/queries/following/FollowMutations.svelte';
	import { getLidarrImportStatusQuery } from '$lib/queries/lidarr-import/LidarrImportQueries.svelte';
	import type { FollowedArtist } from '$lib/queries/following/types';

	const query = getFollowedArtistsQuery();
	const artists = $derived(query.data ?? []);
	const unfollow = createUnfollowMutation();

	const lidarrStatus = getLidarrImportStatusQuery();
	const lidarrConfigured = $derived(lidarrStatus.data?.configured ?? false);
	let importOpen = $state(false);

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

<div class="min-h-[calc(100vh-200px)]">
	<PageHero
		title="Your Artists"
		subtitle="Every name on your follow list — new releases and shows tracked for you."
		eyebrow="Following"
		tint="var(--color-primary)"
		loading={query.isPending}
	>
		{#snippet icon()}
			<Heart class="h-7 w-7" />
		{/snippet}
		{#snippet actions()}
			<a
				href="/following"
				class="btn btn-ghost btn-sm gap-1.5 rounded-full bg-base-content/6"
				aria-label="Back to Following"
			>
				<ArrowLeft class="h-4 w-4" />
				Following
			</a>
			<span
				class={lidarrConfigured ? '' : 'tooltip tooltip-bottom'}
				data-tip={lidarrConfigured ? undefined : 'An admin needs to connect Lidarr first'}
			>
				<button
					class="btn btn-primary btn-sm gap-2 rounded-full"
					disabled={!lidarrConfigured}
					onclick={() => (importOpen = true)}
				>
					<DownloadCloud class="h-4 w-4" aria-hidden="true" />
					<span class="hidden sm:inline">Import from Lidarr</span>
					<span class="sm:hidden">Import</span>
				</button>
			</span>
		{/snippet}
	</PageHero>

	<div class="px-4 pb-12 sm:px-6 lg:px-8">
		<LidarrImportModal bind:open={importOpen} />

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
				ctaHref="/search"
			/>
		{:else}
			<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
				{#each artists as a (a.mbid)}
					{@const chip = stateChip(a)}
					<div
						class="group flex flex-col rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-all hover:-translate-y-0.5 hover:border-primary/30 hover:shadow-lg"
					>
						<div class="relative overflow-hidden rounded-xl">
							<a href="/artist/{a.mbid}" aria-label="Open {a.name}">
								<ArtistImage
									mbid={a.mbid}
									alt={a.name}
									className="w-full aspect-square object-cover transition-transform duration-300 group-hover:scale-105"
								/>
							</a>
							<button
								class="btn btn-circle btn-xs btn-error absolute right-2 top-2 opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100"
								onclick={() => unfollow.mutate(a.mbid)}
								disabled={unfollow.isPending}
								aria-label="Unfollow {a.name}"
								title="Unfollow"
							>
								<X class="h-3.5 w-3.5" />
							</button>
						</div>
						<div class="mt-2.5 min-w-0">
							<a
								href="/artist/{a.mbid}"
								class="block truncate font-display font-semibold tracking-tight hover:text-primary"
								>{a.name}</a
							>
							{#if chip}
								<span class="badge badge-sm {chip.cls} mt-1.5 w-fit">{chip.label}</span>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
