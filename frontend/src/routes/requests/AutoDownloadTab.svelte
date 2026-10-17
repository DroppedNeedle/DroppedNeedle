<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Check, CloudDownload, Heart, Sparkles, TriangleAlert, X } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import {
		getAutoDownloadApprovalBatchesQuery,
		getAutoDownloadApprovalsQuery
	} from '$lib/queries/following/AdminApprovalsQueries.svelte';
	import {
		createApproveAutoDownloadBatchMutation,
		createApproveAutoDownloadMutation,
		createRejectAutoDownloadBatchMutation,
		createRejectAutoDownloadMutation
	} from '$lib/queries/following/AdminApprovalsMutations.svelte';
	import { getPersonalMixApprovalsQuery } from '$lib/queries/scrobble-preferences/PersonalMixApprovalsQuery.svelte';
	import {
		createApprovePersonalMixMutation,
		createRejectPersonalMixMutation
	} from '$lib/queries/scrobble-preferences/ScrobblePreferencesMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import { timeAgo } from './requestItems';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	// Artist auto-download standing approvals.
	const autoApprovalsQuery = getAutoDownloadApprovalsQuery(() => authStore.isAdmin && active);
	const autoApprovals = $derived(autoApprovalsQuery.data?.items ?? []);
	const approveAuto = createApproveAutoDownloadMutation();
	const rejectAuto = createRejectAutoDownloadMutation();

	// Bulk "Lidarr Import" approval batches.
	const batchApprovalsQuery = getAutoDownloadApprovalBatchesQuery(
		() => authStore.isAdmin && active
	);
	const batchApprovals = $derived(batchApprovalsQuery.data?.batches ?? []);
	const approveBatch = createApproveAutoDownloadBatchMutation();
	const rejectBatch = createRejectAutoDownloadBatchMutation();

	// Weekly Mix auto-request standing approvals.
	const mixApprovalsQuery = getPersonalMixApprovalsQuery(() => authStore.isAdmin && active);
	const mixApprovals = $derived(mixApprovalsQuery.data?.items ?? []);
	const approveMix = createApprovePersonalMixMutation();
	const rejectMix = createRejectPersonalMixMutation();

	$effect(() =>
		oncount?.(
			(autoApprovalsQuery.data?.count ?? 0) +
				(batchApprovalsQuery.data?.count ?? 0) +
				(mixApprovalsQuery.data?.count ?? 0)
		)
	);
</script>

{#if active}
	<div in:fade={{ duration: 150 }}>
		{#if autoApprovalsQuery.isError || mixApprovalsQuery.isError || batchApprovalsQuery.isError}
			<div class="alert alert-warning mb-4">
				<TriangleAlert class="h-5 w-5" />
				<span>Could not load auto-download approvals.</span>
				<Button
					size="sm"
					onclick={() => {
						void autoApprovalsQuery.refetch();
						void batchApprovalsQuery.refetch();
						void mixApprovalsQuery.refetch();
					}}>Retry</Button
				>
			</div>
		{:else if autoApprovalsQuery.isPending || mixApprovalsQuery.isPending || batchApprovalsQuery.isPending}
			<div class="flex flex-col gap-2.5">
				{#each Array(3) as _, i (`auto-approval-loading-${i}`)}
					<div
						class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
						style:animation-delay="{i * 100}ms"
					>
						<div class="w-14 h-14 sm:w-16 sm:h-16 bg-base-300 rounded-lg"></div>
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
		{:else if autoApprovals.length === 0 && batchApprovals.length === 0 && mixApprovals.length === 0}
			<RequestsEmpty icon={Heart} tone="success" title="No pending approvals">
				When a user turns on artist auto-download or Weekly Mix auto-requests, it appears here for
				your review.
			</RequestsEmpty>
		{:else}
			<div class="flex flex-col gap-2.5">
				{#each autoApprovals as item (item.user_id + item.artist_mbid)}
					<div in:fly={{ y: 12, duration: 200 }}>
						<Card padding="none">
							<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4">
								<div
									class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
								>
									<ArtistImage
										mbid={item.artist_mbid}
										alt={item.artist_name}
										className="w-full h-full object-cover"
									/>
								</div>
								<div class="flex-1 min-w-0">
									<a
										href={withBasePath(`/artist/${item.artist_mbid}`)}
										class="block font-semibold text-sm truncate hover:text-accent hover:underline"
										title={item.artist_name}>{item.artist_name}</a
									>
									<div class="flex items-center gap-1.5 flex-wrap text-xs text-base-content/40">
										<span>{item.user_name ?? 'A user'}</span>
										<span class="text-base-content/20">•</span>
										<span>requested {timeAgo(item.requested_at)}</span>
									</div>
								</div>
								<div class="flex gap-2 shrink-0">
									<Button
										variant="primary"
										size="sm"
										icon={Check}
										class="gap-1"
										disabled={approveAuto.isPending || rejectAuto.isPending}
										onclick={() =>
											approveAuto.mutate({
												userId: item.user_id,
												mbid: item.artist_mbid,
												artistName: item.artist_name
											})}
									>
										Approve
									</Button>
									<Button
										variant="danger-ghost"
										size="sm"
										icon={X}
										class="gap-1"
										disabled={approveAuto.isPending || rejectAuto.isPending}
										onclick={() =>
											rejectAuto.mutate({
												userId: item.user_id,
												mbid: item.artist_mbid,
												artistName: item.artist_name
											})}
									>
										Reject
									</Button>
								</div>
							</div>
						</Card>
					</div>
				{/each}
				{#each batchApprovals as batch (batch.batch_id)}
					<div in:fly={{ y: 12, duration: 200 }}>
						<Card padding="none">
							<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4">
								<div
									class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg bg-base-300 flex items-center justify-center"
								>
									<CloudDownload class="h-6 w-6 text-accent/60" />
								</div>
								<div class="flex-1 min-w-0">
									<span class="block font-semibold text-sm">
										{batch.user_name ?? 'A user'} wants auto-download on {batch.artist_count} imported
										{batch.artist_count === 1 ? 'artist' : 'artists'}
									</span>
									<div class="flex items-center gap-1.5 flex-wrap text-xs text-base-content/40">
										<span>from Lidarr import</span>
										<span class="text-base-content/20">•</span>
										<span>requested {timeAgo(batch.requested_at)}</span>
									</div>
									{#if batch.sample_names.length > 0}
										<p class="mt-0.5 text-xs text-base-content/50 truncate">
											{batch.sample_names.join(', ')}{batch.artist_count > batch.sample_names.length
												? `, +${batch.artist_count - batch.sample_names.length} more`
												: ''}
										</p>
									{/if}
								</div>
								<div class="flex gap-2 shrink-0">
									<Button
										variant="primary"
										size="sm"
										icon={Check}
										class="gap-1"
										disabled={approveBatch.isPending || rejectBatch.isPending}
										onclick={() =>
											approveBatch.mutate({
												batchId: batch.batch_id,
												userName: batch.user_name ?? 'A user',
												artistCount: batch.artist_count
											})}
									>
										Approve
									</Button>
									<Button
										variant="danger-ghost"
										size="sm"
										icon={X}
										class="gap-1"
										disabled={approveBatch.isPending || rejectBatch.isPending}
										onclick={() =>
											rejectBatch.mutate({
												batchId: batch.batch_id,
												userName: batch.user_name ?? 'A user',
												artistCount: batch.artist_count
											})}
									>
										Reject
									</Button>
								</div>
							</div>
						</Card>
					</div>
				{/each}
				{#each mixApprovals as item (item.user_id)}
					<div in:fly={{ y: 12, duration: 200 }}>
						<Card padding="none">
							<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4">
								<div
									class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg bg-base-300 flex items-center justify-center"
								>
									<Sparkles class="h-6 w-6 text-accent/60" />
								</div>
								<div class="flex-1 min-w-0">
									<span class="block font-semibold text-sm truncate">Weekly Mix auto-requests</span>
									<div class="flex items-center gap-1.5 flex-wrap text-xs text-base-content/40">
										<span>{item.user_name ?? 'A user'}</span>
										<span class="text-base-content/20">•</span>
										<span>requested {timeAgo(item.requested_at)}</span>
									</div>
								</div>
								<div class="flex gap-2 shrink-0">
									<Button
										variant="primary"
										size="sm"
										icon={Check}
										class="gap-1"
										disabled={approveMix.isPending || rejectMix.isPending}
										onclick={() =>
											approveMix.mutate({ userId: item.user_id, userName: item.user_name })}
									>
										Approve
									</Button>
									<Button
										variant="danger-ghost"
										size="sm"
										icon={X}
										class="gap-1"
										disabled={approveMix.isPending || rejectMix.isPending}
										onclick={() =>
											rejectMix.mutate({ userId: item.user_id, userName: item.user_name })}
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
