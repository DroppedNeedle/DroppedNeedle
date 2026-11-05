<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Radar, Trash2 } from 'lucide-svelte';
	import WantedRetryingCard from '$lib/features/requests/components/WantedRetryingCard.svelte';
	import WantedWatchCard from '$lib/features/requests/components/WantedWatchCard.svelte';
	import { getWantedWatchesQuery } from '$lib/features/requests/queries/wanted.svelte';
	import {
		createClearFinishedWatchesMutation,
		createMarkWantedSeenMutation,
		createRemoveWatchMutation,
		createResumeWatchMutation,
		createStopWatchMutation
	} from '$lib/features/requests/queries/wanted.svelte';
	import type { WantedWatchItem } from '$lib/features/requests/queries/wanted.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import RequestsAlert from './RequestsAlert.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import RequestSkeleton from './RequestSkeleton.svelte';
	import './requests.css';

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
	const removeWatch = createRemoveWatchMutation();
	const clearFinished = createClearFinishedWatchesMutation();
	const wantedBusy = $derived(
		stopWatch.isPending || resumeWatch.isPending || removeWatch.isPending
	);

	const finishedCount = $derived(wantedItems.filter((i) => i.state !== 'watching').length);
	let removing = $state<WantedWatchItem | null>(null);
	let removeOpen = $state(false);
	let clearOpen = $state(false);

	function askRemove(item: WantedWatchItem) {
		removing = item;
		removeOpen = true;
	}

	async function confirmRemove() {
		if (!removing) return;
		await removeWatch.mutateAsync({
			mbid: removing.musicbrainz_id,
			albumTitle: removing.album_title
		});
	}

	// A listener only receives their own rows, and the server checks the owner
	// on every action, so each row gets its stop and resume buttons.
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
			<RequestsAlert
				message="Could not load the watchlist."
				onretry={() => void wantedQuery.refetch()}
			/>
		{:else if wantedQuery.isPending}
			<RequestSkeleton count={3} buttons={2} />
		{:else if wantedItems.length === 0 && wantedRetrying.length === 0}
			<RequestsEmpty icon={Radar} tone="muted" title="Nothing on the watchlist">
				When a request can't be found anywhere, DroppedNeedle keeps checking for it and lists it
				here.
			</RequestsEmpty>
		{:else}
			<div class="mb-5 flex flex-wrap items-center justify-between gap-3">
				<p class="rq-intro">
					Albums that couldn't be found are re-checked on a schedule. A copy that passes
					verification downloads by itself; near misses show up as candidates for you to review.
				</p>
				{#if finishedCount > 0}
					<Button
						variant="outline"
						size="display-sm"
						icon={Trash2}
						class="shrink-0"
						onclick={() => (clearOpen = true)}
					>
						Clear finished
					</Button>
				{/if}
			</div>
			<div class="flex flex-col gap-4">
				{#each wantedRetrying as item, i (`retrying-${item.musicbrainz_id}`)}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<WantedRetryingCard {item} ownerName={item.user_name ?? undefined} />
					</div>
				{/each}
				{#each wantedItems as item, i (item.musicbrainz_id)}
					<div in:fly={{ y: 12, duration: 200, delay: (wantedRetrying.length + i) * 30 }}>
						<WantedWatchCard
							{item}
							busy={wantedBusy}
							ownerName={item.user_name ?? undefined}
							onstop={handleWantedStop}
							onresume={handleWantedResume}
							onseen={handleWantedSeen}
							onremove={askRemove}
						/>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}

<ConfirmDialog
	bind:open={removeOpen}
	title="Remove from the list?"
	message={`${removing?.album_title ?? 'This album'} stops being watched. It comes back only if a new request for it fails.`}
	confirmLabel="Remove"
	tone="danger"
	pending={removeWatch.isPending}
	onconfirm={confirmRemove}
/>

<ConfirmDialog
	bind:open={clearOpen}
	title="Clear finished?"
	message={`Removes ${finishedCount} stopped or found ${finishedCount === 1 ? 'album' : 'albums'} from the list.`}
	confirmLabel="Clear"
	tone="danger"
	pending={clearFinished.isPending}
	onconfirm={() => clearFinished.mutateAsync()}
/>
