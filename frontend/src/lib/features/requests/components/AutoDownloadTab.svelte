<script lang="ts">
	import { fade, fly } from '$lib/ui/motion';
	import { Check, CloudDownload, Heart, Sparkles, Undo2, X } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import {
		getAutoDownloadApprovalBatchesQuery,
		getAutoDownloadApprovalsQuery
	} from '$lib/features/following/queries/AdminApprovalsQueries.svelte';
	import {
		createApproveAutoDownloadBatchMutation,
		createApproveAutoDownloadMutation,
		createRejectAutoDownloadBatchMutation,
		createRejectAutoDownloadMutation,
		createRevokeAutoDownloadMutation
	} from '$lib/features/following/queries/AdminApprovalsMutations.svelte';
	import { getPersonalMixApprovalsQuery } from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import {
		createApprovePersonalMixMutation,
		createRejectPersonalMixMutation,
		createRevokePersonalMixMutation
	} from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import RequestRow from './RequestRow.svelte';
	import RequestsAlert from './RequestsAlert.svelte';
	import RequestsEmpty from './RequestsEmpty.svelte';
	import RequestSkeleton from './RequestSkeleton.svelte';
	import { timeAgo } from './requestItems';

	interface Props {
		active: boolean;
		oncount?: (count: number) => void;
	}

	let { active, oncount }: Props = $props();

	// All three load on every admin visit, not only on this tab, so its count is live.
	// Artist auto-download standing approvals.
	const autoApprovalsQuery = getAutoDownloadApprovalsQuery(() => authStore.isAdmin);
	const autoApprovals = $derived(autoApprovalsQuery.data?.items ?? []);
	const approveAuto = createApproveAutoDownloadMutation();
	const grantedAuto = $derived(autoApprovalsQuery.data?.granted ?? []);
	const rejectAuto = createRejectAutoDownloadMutation();
	const revokeAuto = createRevokeAutoDownloadMutation();

	// Bulk "Lidarr Import" approval batches.
	const batchApprovalsQuery = getAutoDownloadApprovalBatchesQuery(() => authStore.isAdmin);
	const batchApprovals = $derived(batchApprovalsQuery.data?.batches ?? []);
	const approveBatch = createApproveAutoDownloadBatchMutation();
	const rejectBatch = createRejectAutoDownloadBatchMutation();

	// Weekly Mix auto-request standing approvals.
	const mixApprovalsQuery = getPersonalMixApprovalsQuery(() => authStore.isAdmin);
	const mixApprovals = $derived(mixApprovalsQuery.data?.items ?? []);
	const approveMix = createApprovePersonalMixMutation();
	const grantedMix = $derived(mixApprovalsQuery.data?.granted ?? []);
	const rejectMix = createRejectPersonalMixMutation();
	const revokeMix = createRevokePersonalMixMutation();

	$effect(() =>
		oncount?.(
			(autoApprovalsQuery.data?.count ?? 0) +
				(batchApprovalsQuery.data?.count ?? 0) +
				(mixApprovalsQuery.data?.count ?? 0)
		)
	);
</script>

{#snippet artistPicture(mbid: string, name: string)}
	<ArtistImage {mbid} alt={name} className="w-full h-full object-cover" />
{/snippet}

{#snippet iconTile(kind: 'batch' | 'mix')}
	<span class="rq-tile">
		{#if kind === 'batch'}
			<CloudDownload class="size-8" aria-hidden="true" />
		{:else}
			<Sparkles class="size-8" aria-hidden="true" />
		{/if}
	</span>
{/snippet}

{#snippet decide(approve: () => void, reject: () => void, busy: boolean)}
	<div class="flex flex-wrap justify-end gap-2.5">
		<Button variant="primary" size="display-sm" icon={Check} disabled={busy} onclick={approve}>
			Approve
		</Button>
		<Button variant="danger-ghost" size="display-sm" icon={X} disabled={busy} onclick={reject}>
			Reject
		</Button>
	</div>
{/snippet}

{#snippet revoke(onclick: () => void, busy: boolean)}
	<Button variant="outline" size="display-sm" icon={Undo2} disabled={busy} {onclick}>Revoke</Button>
{/snippet}

{#if active}
	<div in:fade={{ duration: 150 }}>
		{#if autoApprovalsQuery.isError || mixApprovalsQuery.isError || batchApprovalsQuery.isError}
			<RequestsAlert
				message="Could not load auto-download approvals."
				onretry={() => {
					void autoApprovalsQuery.refetch();
					void batchApprovalsQuery.refetch();
					void mixApprovalsQuery.refetch();
				}}
			/>
		{:else if autoApprovalsQuery.isPending || mixApprovalsQuery.isPending || batchApprovalsQuery.isPending}
			<RequestSkeleton count={3} buttons={2} />
		{:else if autoApprovals.length === 0 && batchApprovals.length === 0 && mixApprovals.length === 0 && grantedAuto.length === 0 && grantedMix.length === 0}
			<RequestsEmpty icon={Heart} tone="success" title="No pending approvals">
				When a user turns on artist auto-download or Weekly Mix auto-requests, it appears here for
				your review.
			</RequestsEmpty>
		{:else}
			<div class="flex flex-col gap-4">
				{#each autoApprovals as item (item.user_id + item.artist_mbid)}
					{@const busy = approveAuto.isPending || rejectAuto.isPending}
					{@const args = {
						userId: item.user_id,
						mbid: item.artist_mbid,
						artistName: item.artist_name
					}}
					<div in:fly={{ y: 12, duration: 200 }}>
						<RequestRow round>
							{#snippet cover()}{@render artistPicture(
									item.artist_mbid,
									item.artist_name
								)}{/snippet}
							{#snippet aside()}{@render decide(
									() => approveAuto.mutate(args),
									() => rejectAuto.mutate(args),
									busy
								)}{/snippet}
							<a
								href={withBasePath(`/artist/${item.artist_mbid}`)}
								class="rq-title"
								title={item.artist_name}>{item.artist_name}</a
							>
							<p class="rq-meta mt-1.5">
								<span>{item.user_name ?? 'A user'}</span>
								<span>requested {timeAgo(item.requested_at)}</span>
							</p>
						</RequestRow>
					</div>
				{/each}
				{#each batchApprovals as batch (batch.batch_id)}
					{@const busy = approveBatch.isPending || rejectBatch.isPending}
					{@const args = {
						batchId: batch.batch_id,
						userName: batch.user_name ?? 'A user',
						artistCount: batch.artist_count
					}}
					<div in:fly={{ y: 12, duration: 200 }}>
						<RequestRow>
							{#snippet cover()}{@render iconTile('batch')}{/snippet}
							{#snippet aside()}{@render decide(
									() => approveBatch.mutate(args),
									() => rejectBatch.mutate(args),
									busy
								)}{/snippet}
							<p class="rq-title rq-title--sm">
								{batch.user_name ?? 'A user'} wants auto-download on {batch.artist_count} imported
								{batch.artist_count === 1 ? 'artist' : 'artists'}
							</p>
							<p class="rq-meta mt-1.5">
								<span>from Lidarr import</span>
								<span>requested {timeAgo(batch.requested_at)}</span>
							</p>
							{#if batch.sample_names.length > 0}
								<p class="mt-1.5 truncate text-sm text-muted">
									{batch.sample_names.join(', ')}{batch.artist_count > batch.sample_names.length
										? `, +${batch.artist_count - batch.sample_names.length} more`
										: ''}
								</p>
							{/if}
						</RequestRow>
					</div>
				{/each}
				{#each mixApprovals as item (item.user_id)}
					{@const busy = approveMix.isPending || rejectMix.isPending}
					{@const args = { userId: item.user_id, userName: item.user_name }}
					<div in:fly={{ y: 12, duration: 200 }}>
						<RequestRow>
							{#snippet cover()}{@render iconTile('mix')}{/snippet}
							{#snippet aside()}{@render decide(
									() => approveMix.mutate(args),
									() => rejectMix.mutate(args),
									busy
								)}{/snippet}
							<p class="rq-title">Weekly Mix auto-requests</p>
							<p class="rq-meta mt-1.5">
								<span>{item.user_name ?? 'A user'}</span>
								<span>requested {timeAgo(item.requested_at)}</span>
							</p>
						</RequestRow>
					</div>
				{/each}
				{#if grantedAuto.length > 0 || grantedMix.length > 0}
					<h3 class="rq-approved">Approved</h3>
				{/if}
				{#each grantedAuto as item (item.user_id + item.artist_mbid)}
					<RequestRow round>
						{#snippet cover()}{@render artistPicture(item.artist_mbid, item.artist_name)}{/snippet}
						{#snippet aside()}{@render revoke(
								() =>
									revokeAuto.mutate({
										userId: item.user_id,
										mbid: item.artist_mbid,
										artistName: item.artist_name
									}),
								revokeAuto.isPending
							)}{/snippet}
						<a
							href={withBasePath(`/artist/${item.artist_mbid}`)}
							class="rq-title"
							title={item.artist_name}>{item.artist_name}</a
						>
						<p class="rq-meta mt-1.5"><span>{item.user_name ?? 'A user'}</span></p>
					</RequestRow>
				{/each}
				{#each grantedMix as item (item.user_id)}
					<RequestRow>
						{#snippet cover()}{@render iconTile('mix')}{/snippet}
						{#snippet aside()}{@render revoke(
								() => revokeMix.mutate({ userId: item.user_id, userName: item.user_name }),
								revokeMix.isPending
							)}{/snippet}
						<p class="rq-title">Weekly Mix auto-requests</p>
						<p class="rq-meta mt-1.5"><span>{item.user_name}</span></p>
					</RequestRow>
				{/each}
			</div>
		{/if}
	</div>
{/if}

<style>
	.rq-tile {
		display: grid;
		place-items: center;
		width: 100%;
		height: 100%;
		background-color: var(--color-neutral);
		color: var(--dn-title);
	}
	.rq-title--sm {
		font-size: 1.375rem;
	}
	.rq-approved {
		margin-top: 1rem;
		font-family: var(--font-display);
		font-size: 1.625rem;
		font-weight: 400;
		letter-spacing: 0.015em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	@media (max-width: 639px) {
		.rq-title--sm {
			font-size: 1.125rem;
		}
	}
</style>
