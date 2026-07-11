<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { Disc3, Plus, Check, ArrowLeft } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import { getRecentReleasesQuery } from '$lib/queries/following/FollowQueries.svelte';
	import { createMarkNewReleasesSeenMutation } from '$lib/queries/following/FollowMutations.svelte';
	import { requestAlbum } from '$lib/queries/downloads/DownloadMutations.svelte';
	import type { NewRelease } from '$lib/queries/following/types';
	import { SvelteSet } from 'svelte/reactivity';

	const PAGE = 48;
	const MAX_LIMIT = 480; // the backend caps the recent endpoint at 500
	const PERIODS = [
		{ days: 30, label: 'Last 30 days' },
		{ days: 90, label: 'Last 90 days' },
		{ days: 365, label: 'Last year' }
	];

	let limit = $state(PAGE);
	let days = $state(30);
	let hideOwned = $state(false);

	// the release LOG: owned albums included (with a tick) unless hidden
	const query = getRecentReleasesQuery(
		() => days,
		() => limit,
		() => !hideOwned
	);
	const items = $derived(query.data?.items ?? []);
	const total = $derived(query.data?.total ?? 0);

	const request = requestAlbum();
	let requested = new SvelteSet<string>();

	// visiting this page is what clears the sidebar new-releases badge
	const markSeen = createMarkNewReleasesSeenMutation();
	onMount(() => markSeen.mutate(undefined));

	function onRequest(item: NewRelease) {
		request.mutate({
			release_group_mbid: item.release_group_mbid,
			artist_name: item.artist_name,
			album_title: item.title,
			artist_mbid: item.artist_mbid
		});
		requested.add(item.release_group_mbid);
	}
</script>

<svelte:head>
	<title>New Releases - DroppedNeedle</title>
</svelte:head>

<div class="mx-auto w-full max-w-6xl px-2 py-4 sm:px-4 sm:py-8 lg:px-8">
	<div class="mb-6 flex flex-wrap items-center gap-3">
		<a
			href={resolve('/following')}
			class="btn btn-ghost btn-sm btn-circle"
			aria-label="Back to Following"
		>
			<ArrowLeft class="h-5 w-5" />
		</a>
		<Disc3 class="h-6 w-6 text-primary" aria-hidden="true" />
		<h1 class="text-2xl font-bold sm:text-3xl">New Releases</h1>

		<div class="ml-auto flex items-center gap-3">
			<label class="flex cursor-pointer items-center gap-2 text-sm text-base-content/70">
				<input type="checkbox" class="toggle toggle-sm toggle-primary" bind:checked={hideOwned} />
				Hide owned
			</label>
			<select class="select select-sm select-bordered" bind:value={days} aria-label="Time period">
				{#each PERIODS as period (period.days)}
					<option value={period.days}>{period.label}</option>
				{/each}
			</select>
		</div>
	</div>

	{#if query.isPending}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
			{#each Array(10) as _, i (i)}
				<div class="aspect-square w-full animate-pulse rounded-2xl bg-base-200"></div>
			{/each}
		</div>
	{:else if items.length === 0}
		<EmptyState
			icon={Disc3}
			title={hideOwned ? 'Nothing left to grab' : 'Nothing released in this period'}
			description={hideOwned
				? 'Every release from this period is already in your library.'
				: 'When an artist you follow puts out something new, it shows up here.'}
			ctaLabel="Your Artists"
			ctaHref="/following/artists"
		/>
	{:else}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
			{#each items as item (item.release_group_mbid)}
				{@const isRequested = requested.has(item.release_group_mbid)}
				<div class="group flex flex-col gap-2">
					<a
						href={resolve(`/album/${item.release_group_mbid}`)}
						class="relative block aspect-square w-full overflow-hidden rounded-2xl"
						aria-label="Open {item.title}"
					>
						<AlbumImage
							mbid={item.release_group_mbid}
							alt={item.title}
							size="full"
							requestSize={250}
							rounded="xl"
							className="h-full w-full transition-transform duration-300 group-hover:scale-105"
						/>
						{#if item.in_library}
							<span
								class="badge badge-success badge-sm absolute top-2 right-2 gap-0.5 border-0 font-medium"
								title="In your library"
							>
								<Check class="h-3 w-3" aria-hidden="true" />
								<span class="sr-only">In your library</span>
							</span>
						{/if}
					</a>
					<a
						href={resolve(`/album/${item.release_group_mbid}`)}
						class="truncate font-semibold hover:underline"
					>
						{item.title}
					</a>
					<a
						href={resolve(`/artist/${item.artist_mbid}`)}
						class="truncate text-sm text-base-content/70 hover:underline"
					>
						{item.artist_name}
					</a>
					<div class="flex items-center justify-between gap-2">
						{#if item.first_release_date}
							<span class="text-xs text-base-content/50 tabular-nums"
								>{item.first_release_date}</span
							>
						{:else}
							<span></span>
						{/if}
						{#if item.in_library}
							<span class="text-xs font-medium text-success">In library</span>
						{:else}
							<button
								class="btn btn-xs gap-1 {isRequested ? 'btn-success btn-outline' : 'btn-accent'}"
								onclick={() => onRequest(item)}
								disabled={isRequested}
							>
								{#if isRequested}
									<Check class="h-3.5 w-3.5" /> Requested
								{:else}
									<Plus class="h-3.5 w-3.5" /> Request
								{/if}
							</button>
						{/if}
					</div>
				</div>
			{/each}
		</div>

		{#if items.length < total && limit < MAX_LIMIT}
			<div class="mt-6 flex justify-center">
				<button
					class="btn btn-outline"
					onclick={() => (limit = Math.min(limit + PAGE, MAX_LIMIT))}
					disabled={query.isFetching}
				>
					{query.isFetching ? 'Loading...' : `Load more (${total - items.length} more)`}
				</button>
			</div>
		{/if}
	{/if}
</div>
