<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Radar, TriangleAlert } from 'lucide-svelte';
	import WantedRetryingCard from '$lib/components/WantedRetryingCard.svelte';
	import WantedWatchCard from '$lib/components/WantedWatchCard.svelte';
	import { getWantedWatchesQuery } from '$lib/queries/wanted/WantedQuery.svelte';
	import {
		createMarkWantedSeenMutation,
		createResumeWatchMutation,
		createStopWatchMutation
	} from '$lib/queries/wanted/WantedMutations.svelte';
	import type { WantedWatchItem } from '$lib/queries/wanted/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	// Availability re-search watchlist, refreshed by the wanted_* SSE events.
	const wantedQuery = getWantedWatchesQuery(() => active);
	const wantedItems = $derived(wantedQuery.data?.items ?? []);
	const wantedRetrying = $derived(wantedQuery.data?.retrying ?? []);

	$effect(() =>
		oncount?.(wantedItems.filter((i) => i.state === 'watching').length + wantedRetrying.length)
	);

	const stopWatch = createStopWatchMutation();
	const resumeWatch = createResumeWatchMutation();
	const markWantedSeen = createMarkWantedSeenMutation();
	const wantedBusy = $derived(stopWatch.isPending || resumeWatch.isPending);

	function handleWantedStop(item: WantedWatchItem) {
		stopWatch.mutate({ mbid: item.musicbrainz_id, albumTitle: item.album_title });
	}

	function handleWantedResume(item: WantedWatchItem) {
		resumeWatch.mutate({ mbid: item.musicbrainz_id, albumTitle: item.album_title });
	}

	function handleWantedSeen(item: WantedWatchItem) {
		markWantedSeen.mutate({ mbid: item.musicbrainz_id, albumTitle: item.album_title });
	}
</script>

{#if active}
	<div in:fade={{ duration: 150 }}>
		{#if wantedQuery.isError}
			<div class="alert alert-warning mb-4">
				<TriangleAlert class="h-5 w-5" />
				<span>Could not load the watchlist.</span>
				<Button size="sm" onclick={() => void wantedQuery.refetch()}>Retry</Button>
			</div>
		{:else if wantedQuery.isPending}
			<div class="flex flex-col gap-2.5">
				{#each Array(3) as _, i (`wanted-loading-${i}`)}
					<div
						class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
						style:animation-delay="{i * 100}ms"
					>
						<div class="w-14 h-14 sm:w-16 sm:h-16 bg-base-300 rounded-lg"></div>
						<div class="flex-1">
							<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
							<div class="h-3 bg-base-300 rounded w-28 mb-1"></div>
							<div class="h-2.5 bg-base-300 rounded w-52"></div>
						</div>
						<div class="flex gap-2">
							<div class="h-8 bg-base-300 rounded-btn w-24"></div>
							<div class="h-8 bg-base-300 rounded-btn w-16"></div>
						</div>
					</div>
				{/each}
			</div>
		{:else if wantedItems.length === 0 && wantedRetrying.length === 0}
			<RequestsEmpty icon={Radar} tone="muted" title="Nothing on the watchlist">
				When a request can't be found anywhere, DroppedNeedle keeps checking for it and lists it
				here.
			</RequestsEmpty>
		{:else}
			<p class="text-xs text-base-content/40 mb-3">
				Albums that couldn't be found are re-checked on a schedule. A copy that passes verification
				downloads by itself; near misses show up as candidates for you to review.
			</p>
			<div class="flex flex-col gap-2.5">
				{#each wantedRetrying as item, i (`retrying-${item.musicbrainz_id}`)}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<WantedRetryingCard {item} />
					</div>
				{/each}
				{#each wantedItems as item, i (item.musicbrainz_id)}
					<div in:fly={{ y: 12, duration: 200, delay: (wantedRetrying.length + i) * 30 }}>
						<WantedWatchCard
							{item}
							busy={wantedBusy}
							onstop={authStore.isAdmin || item.user_id === authStore.user?.id
								? handleWantedStop
								: undefined}
							onresume={authStore.isAdmin || item.user_id === authStore.user?.id
								? handleWantedResume
								: undefined}
							onseen={handleWantedSeen}
						/>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}
