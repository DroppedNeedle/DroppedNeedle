<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Check, CircleCheck, TriangleAlert, X } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getApprovalsQuery } from '$lib/queries/requests/RequestQueries.svelte';
	import {
		createApproveRequestMutation,
		createRejectRequestMutation
	} from '$lib/queries/requests/RequestMutations.svelte';
	import type { RequestItem, RequestKind } from '$lib/queries/requests/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
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
			<div class="alert alert-warning mb-4">
				<TriangleAlert class="h-5 w-5" />
				<span>{approvalError}</span>
				<Button size="sm" onclick={() => void approvalsQuery.refetch()}>Retry</Button>
			</div>
		{/if}

		{#if approvalLoading && approvalItems.length === 0}
			<div class="flex flex-col gap-2.5">
				{#each Array(3) as _, i (`approval-loading-${i}`)}
					<div
						class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
						style:animation-delay="{i * 100}ms"
					>
						<div class="w-14 h-14 sm:w-18 sm:h-18 bg-base-300 rounded-lg"></div>
						<div class="flex-1">
							<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
							<div class="h-3 bg-base-300 rounded w-28"></div>
						</div>
						<div class="flex gap-2">
							<div class="h-8 bg-base-300 rounded-btn w-20"></div>
							<div class="h-8 bg-base-300 rounded-btn w-20"></div>
						</div>
					</div>
				{/each}
			</div>
		{:else if approvalItems.length === 0}
			<RequestsEmpty icon={CircleCheck} tone="success" title="No pending approvals">
				Requests from regular users will appear here for your review.
			</RequestsEmpty>
		{:else}
			<div class="flex flex-col gap-2.5">
				{#each approvalItems as item, i (itemRequestKey(item))}
					<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
						<Card padding="none">
							<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4">
								<div
									class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
								>
									<AlbumImage
										mbid={itemKind(item) === 'track' ? '' : item.musicbrainz_id}
										customUrl={null}
										alt={item.album_title}
										size="sm"
										rounded="lg"
										className="w-full h-full"
									/>
								</div>
								<div class="flex-1 min-w-0">
									{#if itemKind(item) === 'track'}
										<div class="flex items-center gap-1.5 min-w-0">
											<span class="badge badge-ghost badge-xs shrink-0">Track</span>
											<p class="font-semibold text-sm truncate">
												{item.track_title ?? 'Unknown track'}
											</p>
										</div>
										<p class="text-base-content/60 text-xs truncate">
											Album: {item.album_title}
										</p>
									{:else}
										<p class="font-semibold text-sm truncate">{item.album_title}</p>
									{/if}
									<p class="text-base-content/60 text-xs truncate">{item.artist_name}</p>
									<div class="flex items-center gap-1.5 flex-wrap">
										{#if item.year}
											<span class="text-base-content/40 text-xs">{item.year}</span>
										{/if}
										{#if item.requested_by_name}
											{#if item.year}<span class="text-base-content/20 text-xs">•</span>{/if}
											<span class="text-base-content/40 text-xs">
												Requested by {item.requested_by_name}
											</span>
										{/if}
									</div>
								</div>
								<div class="flex gap-2 shrink-0">
									<Button
										variant="primary"
										size="sm"
										icon={Check}
										class="gap-1"
										onclick={() => void handleApprove(item.musicbrainz_id, itemKind(item))}
									>
										Approve
									</Button>
									<Button
										variant="danger-ghost"
										size="sm"
										icon={X}
										class="gap-1"
										onclick={() => void handleReject(item.musicbrainz_id, itemKind(item))}
									>
										Reject
									</Button>
								</div>
							</div>
						</Card>
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}
