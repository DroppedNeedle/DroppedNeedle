<script lang="ts">
	import { onMount } from 'svelte';
	import { Disc3, Plus, Check } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import Button from '$lib/ui/Button.svelte';
	import FollowEmpty from '$lib/features/following/components/FollowEmpty.svelte';
	import { getRecentReleasesQuery } from '$lib/features/following/queries/FollowQueries.svelte';
	import { createMarkNewReleasesSeenMutation } from '$lib/features/following/queries/FollowMutations.svelte';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import type { NewRelease } from '$lib/features/following/queries/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { SvelteSet } from 'svelte/reactivity';

	const PAGE = 48;
	const MAX_LIMIT = 480; // the backend caps the recent endpoint at 500
	const PERIODS: { value: '30' | '90' | '365'; label: string }[] = [
		{ value: '30', label: 'Last 30 days' },
		{ value: '90', label: 'Last 90 days' },
		{ value: '365', label: 'Last year' }
	];

	let limit = $state(PAGE);
	let period = $state<'30' | '90' | '365'>('30');
	let hideOwned = $state(false);

	// the release LOG: owned albums included (with a tick) unless hidden
	const query = getRecentReleasesQuery(
		() => Number(period),
		() => limit,
		() => !hideOwned
	);
	const items = $derived(query.data?.items ?? []);
	const total = $derived(query.data?.total ?? 0);

	const request = createRequestAlbumMutation();
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

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader
		title="New Releases"
		ticket="DN / CC 3.2 - ALB"
		backHref={withBasePath('/following')}
		backLabel="Back to Following"
	>
		{#snippet actions()}
			<div class="nr-controls">
				<label class="nr-hide">
					<span>Hide owned</span>
					<ToggleRow bare label="Hide owned" bind:checked={hideOwned} />
				</label>
				<div class="nr-period">
					<SelectField label="Time period" size="sm" options={PERIODS} bind:value={period} />
				</div>
			</div>
		{/snippet}
	</ListPageHeader>

	<div class="px-4 pb-10 sm:px-6 lg:px-8">
		{#if query.isPending}
			<div class="dn-list-grid">
				{#each Array(10) as _, i (i)}
					<MediaTileSkeleton />
				{/each}
			</div>
		{:else if items.length === 0}
			<div class="nr-empty">
				<FollowEmpty
					icon={Disc3}
					titleAs="h2"
					title={hideOwned ? 'Nothing left to grab' : 'Nothing released in this period'}
					description={hideOwned
						? 'Every release from this period is already in your library.'
						: 'When an artist you follow puts out something new, it shows up here.'}
					ctaLabel="Your Artists"
					ctaHref={withBasePath('/following/artists')}
				/>
			</div>
		{:else}
			<div class="dn-list-grid">
				{#each items as item (item.release_group_mbid)}
					{@const isRequested = requested.has(item.release_group_mbid)}
					<div class="nr-item">
						<MediaTile
							title={item.title}
							titleAs="h2"
							href={albumHref(item.release_group_mbid)}
							label="Open {item.title}"
						>
							{#snippet art()}
								<AlbumImage
									mbid={item.release_group_mbid}
									alt={item.title}
									size="full"
									requestSize={250}
									className="h-full w-full"
								/>
							{/snippet}
							{#snippet overlay()}
								{#if item.in_library}
									<span class="nr-tick" title="In your library">
										<Check class="size-3.5" strokeWidth={3} aria-hidden="true" />
									</span>
								{/if}
							{/snippet}
						</MediaTile>
						<div class="nr-item__foot">
							<a href={artistHref(item.artist_mbid)} class="nr-item__artist">
								{item.artist_name}
							</a>
							<div class="nr-item__row">
								<span class="nr-item__date">{item.first_release_date ?? ''}</span>
								{#if item.in_library}
									<span class="nr-item__owned">
										<Check class="size-3.5" strokeWidth={2.5} aria-hidden="true" />In library
									</span>
								{:else}
									<Button
										variant={isRequested ? 'outline' : 'primary'}
										size="xs"
										icon={isRequested ? Check : Plus}
										disabled={isRequested}
										onclick={() => onRequest(item)}
									>
										{isRequested ? 'Requested' : 'Request'}
									</Button>
								{/if}
							</div>
						</div>
					</div>
				{/each}
			</div>

			{#if items.length < total && limit < MAX_LIMIT}
				<div class="mt-8 flex justify-center">
					<Button
						variant="outline"
						size="display-sm"
						disabled={query.isFetching}
						onclick={() => (limit = Math.min(limit + PAGE, MAX_LIMIT))}
					>
						{query.isFetching ? 'Loading...' : `Load more (${total - items.length} more)`}
					</Button>
				</div>
			{/if}
		{/if}
	</div>
</div>

<style>
	.nr-controls {
		display: flex;
		align-items: flex-end;
		gap: 1.5rem;
	}
	.nr-hide {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-height: 2rem;
		margin-bottom: 0.25rem;
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.nr-period {
		width: 11.5rem;
	}
	.nr-period :global(.fieldset) {
		padding: 0;
	}
	.nr-period :global(.fieldset-legend) {
		padding: 0 0 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--color-muted);
	}

	.nr-empty {
		max-width: 46rem;
		margin-inline: auto;
		padding-top: 0.5rem;
	}

	.nr-tick {
		position: absolute;
		top: 0.375rem;
		right: 0.375rem;
		display: grid;
		place-items: center;
		width: 1.5rem;
		height: 1.5rem;
		border: 1px solid var(--dn-action-edge);
		border-radius: 9999px;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: 0 2px 6px rgb(0 0 0 / 0.35);
	}

	.nr-item {
		min-width: 0;
	}
	.nr-item__foot {
		padding: 0 0.25rem;
	}
	.nr-item__artist {
		display: block;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--color-muted);
		text-overflow: ellipsis;
		white-space: nowrap;
		transition: color 150ms;
	}
	.nr-item__artist:hover {
		color: var(--dn-title);
		text-decoration: underline;
	}
	.nr-item__row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.375rem 0.5rem;
		min-height: 1.5rem;
		margin-top: 0.5rem;
	}
	.nr-item__owned {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--color-success);
	}
	.nr-item__date {
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.04em;
		color: var(--color-muted);
		font-variant-numeric: tabular-nums;
	}

	@media (max-width: 767px) {
		.nr-controls {
			grid-column: 1 / -1;
			justify-content: space-between;
			gap: 1rem;
		}
		.nr-period {
			width: 10rem;
		}
	}
</style>
