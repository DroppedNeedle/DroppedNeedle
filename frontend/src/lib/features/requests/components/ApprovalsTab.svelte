<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Check, ShieldCheck, X } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { getApprovalsQuery } from '$lib/features/requests/queries/RequestQueries.svelte';
	import {
		createApproveRequestMutation,
		createRejectRequestMutation
	} from '$lib/features/requests/queries/RequestMutations.svelte';
	import type { RequestItem, RequestKind } from '$lib/features/requests/queries/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import RequestRow from './RequestRow.svelte';
	import RequestsAlert from './RequestsAlert.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import RequestSkeleton from './RequestSkeleton.svelte';
	import { itemKind, itemRequestKey } from './requestItems';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	// Fetched on every admin visit, not only on this tab, so its badge is live.
	const approvalsQuery = getApprovalsQuery(() => authStore.isAdmin);
	const approvalItems = $derived<RequestItem[]>(approvalsQuery.data?.items ?? []);
	const approvalLoading = $derived(approvalsQuery.isPending);
	const approvalError = $derived(approvalsQuery.isError ? "Couldn't load pending approvals" : null);

	$effect(() => oncount?.(approvalsQuery.data?.count ?? approvalItems.length));

	const approveRequestMutation = createApproveRequestMutation();
	const rejectRequestMutation = createRejectRequestMutation();

	async function handleApprove(mbid: string, requestKind: RequestKind) {
		try {
			await approveRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	async function handleReject(mbid: string, requestKind: RequestKind) {
		try {
			await rejectRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}
</script>

{#if active}
	<div in:fade={{ duration: 150 }}>
		{#if approvalError}
			<RequestsAlert message={approvalError} onretry={() => void approvalsQuery.refetch()} />
		{/if}

		{#if approvalLoading && approvalItems.length === 0}
			<RequestSkeleton count={3} buttons={2} />
		{:else if approvalItems.length === 0}
			<RequestsEmpty icon={ShieldCheck} tone="success" title="No pending approvals">
				Requests from regular users will appear here for your review.
			</RequestsEmpty>
		{:else}
			<div class="flex flex-col gap-4">
				{#each approvalItems as item, i (itemRequestKey(item))}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<RequestRow>
							{#snippet cover()}
								<AlbumImage
									mbid={itemKind(item) === 'track'
										? (item.track_release_group_mbid ?? '')
										: item.musicbrainz_id}
									customUrl={null}
									alt={item.album_title}
									size="sm"
									rounded="none"
									className="w-full h-full"
								/>
							{/snippet}
							{#snippet aside()}
								<div class="flex flex-wrap justify-end gap-2.5">
									<Button
										variant="primary"
										size="display-sm"
										icon={Check}
										onclick={() => void handleApprove(item.musicbrainz_id, itemKind(item))}
									>
										Approve
									</Button>
									<Button
										variant="danger-ghost"
										size="display-sm"
										icon={X}
										onclick={() => void handleReject(item.musicbrainz_id, itemKind(item))}
									>
										Reject
									</Button>
								</div>
							{/snippet}
							{#if itemKind(item) === 'track'}
								<div class="flex min-w-0 items-center gap-2.5">
									<span class="rq-pill rq-pill--neutral rq-pill--outline">Track</span>
									<p class="rq-title">{item.track_title ?? 'Unknown track'}</p>
								</div>
								<p class="truncate text-sm text-muted">Album: {item.album_title}</p>
							{:else}
								<p class="rq-title">{item.album_title}</p>
							{/if}
							<p class="rq-artist">{item.artist_name}</p>
							<p class="rq-meta mt-2">
								{#if item.year}
									<span>Year {item.year}</span>
								{/if}
								{#if item.requested_by_name}
									<span>Requested by {item.requested_by_name}</span>
								{/if}
							</p>
						</RequestRow>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}
