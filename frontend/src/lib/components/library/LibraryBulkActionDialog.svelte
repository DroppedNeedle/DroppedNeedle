<script lang="ts">
	import { CirclePause, CirclePlay, OctagonX } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryOperationQuery } from '$lib/queries/library/LibraryOperationQueries.svelte';
	import { controlLibraryOperation } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import {
		applyBulkLibraryReview,
		previewBulkLibraryReview
	} from '$lib/queries/library/LibraryReviewMutations.svelte';
	import type {
		BulkReviewAction,
		OperationResponse,
		ReviewListItem
	} from '$lib/queries/library/LibraryOperationsTypes';
	import type { LibraryReviewFilters } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		selected: ReviewListItem[];
		allMatching: boolean;
		matchingCount: number;
		filters: LibraryReviewFilters;
		catalogRevision: number;
		onclear: () => void;
	}
	let { selected, allMatching, matchingCount, filters, catalogRevision, onclear }: Props = $props();
	const preview = previewBulkLibraryReview();
	const apply = applyBulkLibraryReview();
	const pause = controlLibraryOperation('pause');
	const resume = controlLibraryOperation('resume');
	const stop = controlLibraryOperation('stop');
	let dialog: HTMLDialogElement;
	let action = $state<BulkReviewAction>('keep_tagged');
	let candidateKey = $state<string | null>(null);
	let activeJobId = $state<string | null>(null);
	const operationQuery = getLibraryOperationQuery(() => activeJobId);
	const operationFinished = $derived(
		operationQuery.data !== undefined &&
			['succeeded', 'failed', 'cancelled', 'stopped'].includes(operationQuery.data.state)
	);
	let dialogHeading: HTMLHeadingElement;
	let opener: HTMLButtonElement | null = null;
	const storageKey = $derived(
		`droppedneedle:library-bulk-job:${authStore.user?.id ?? 'anonymous'}`
	);

	$effect(() => {
		if (typeof sessionStorage === 'undefined') return;
		activeJobId = sessionStorage.getItem(storageKey);
	});

	$effect(() => {
		if (!operationFinished || typeof sessionStorage === 'undefined') return;
		if (sessionStorage.getItem(storageKey) === activeJobId) {
			sessionStorage.removeItem(storageKey);
		}
	});

	const selection = $derived({
		review_ids: allMatching ? [] : selected.map((item) => item.id),
		expected_revisions: allMatching
			? {}
			: Object.fromEntries(selected.map((item) => [item.id, item.row_revision])),
		normalized_filter: Object.fromEntries(
			Object.entries(filters)
				.filter(([key]) => !['cursor', 'sort'].includes(key))
				.filter(([, value]) => value !== undefined && value !== '')
				.map(([key, value]) => [
					key === 'reasonCode' ? 'reason_code' : key === 'rootId' ? 'root_id' : key,
					String(value)
				])
		),
		catalog_revision: catalogRevision
	});

	async function openPreview(
		nextAction: BulkReviewAction,
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): Promise<void> {
		opener = event.currentTarget;
		action = nextAction;
		candidateKey = null;
		try {
			await preview.mutateAsync({ action, selection, candidate_key: null });
		} catch {
			// The dialog shows the failed preview without implying that anything changed.
		}
		dialog.showModal();
		dialogHeading.focus();
	}

	async function previewCandidate(key: string): Promise<void> {
		candidateKey = key || null;
		if (!candidateKey) return;
		try {
			await preview.mutateAsync({
				action: 'accept_candidate',
				selection,
				candidate_key: candidateKey
			});
		} catch {
			// Keep the dialog open so the mutation error remains visible.
		}
	}

	async function applyPreview(): Promise<void> {
		if (!preview.data) return;
		let job: OperationResponse;
		try {
			job = await apply.mutateAsync({
				preview_token: preview.data.preview_token,
				idempotency_key: createUuid(),
				action,
				selection,
				candidate_key: candidateKey,
				confirm_local_metadata: preview.data.requires_local_metadata_confirmation
			});
		} catch {
			return;
		}
		activeJobId = job.id;
		try {
			sessionStorage.setItem(storageKey, job.id);
		} catch {
			// The durable server job still recovers through activity when storage is unavailable.
		}
		dialog.close();
		onclear();
	}
</script>

{#if selected.length > 0 || allMatching}
	<div
		class="sticky bottom-4 z-30 mx-auto flex max-w-3xl flex-wrap items-center gap-2 rounded-box border border-primary/25 bg-base-100/95 p-3 shadow-xl backdrop-blur"
		role="toolbar"
		aria-label="Bulk review actions"
	>
		<strong class="mr-auto"
			>{allMatching
				? `All ${matchingCount.toLocaleString()} matching selected`
				: `${selected.length.toLocaleString()} on this page selected`}</strong
		>
		<button
			class="btn btn-outline btn-sm"
			disabled={preview.isPending}
			onclick={(event) => void openPreview('keep_tagged', event)}>Keep as tagged...</button
		>
		<button
			class="btn btn-outline btn-sm"
			disabled={preview.isPending}
			onclick={(event) => void openPreview('retry', event)}>Retry...</button
		>
		<button
			class="btn btn-outline btn-sm"
			disabled={preview.isPending}
			onclick={(event) => void openPreview('accept_candidate', event)}
			>Accept shared candidate...</button
		>
		<button
			class="btn btn-outline btn-error btn-sm"
			disabled={preview.isPending}
			onclick={(event) => void openPreview('exclude', event)}>Exclude...</button
		>
		<button class="btn btn-ghost btn-sm" onclick={onclear}>Clear</button>
	</div>
{/if}

{#if operationQuery.data}
	{@const job = operationQuery.data}
	<div class="mt-4 rounded-box border border-primary/20 bg-base-100 p-4" role="status">
		<div class="flex flex-wrap items-center gap-2">
			<strong class="mr-auto">Bulk review · {job.state}</strong>{#if job.state === 'running'}<button
					class="btn btn-ghost btn-xs"
					onclick={() =>
						void pause
							.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
							.catch(() => undefined)}
					aria-label="Pause bulk review"><CirclePause class="h-3.5 w-3.5" /> Pause</button
				>{:else if job.state === 'paused'}<button
					class="btn btn-ghost btn-xs"
					onclick={() =>
						void resume
							.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
							.catch(() => undefined)}
					aria-label="Resume bulk review"><CirclePlay class="h-3.5 w-3.5" /> Resume</button
				>{/if}{#if ['queued', 'running', 'paused'].includes(job.state)}<button
					class="btn btn-ghost btn-xs text-error"
					onclick={() =>
						void stop
							.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
							.catch(() => undefined)}
					aria-label="Stop bulk review"><OctagonX class="h-3.5 w-3.5" /> Stop</button
				>{/if}{#if operationFinished}<button
					class="btn btn-ghost btn-xs"
					onclick={() => (activeJobId = null)}>Dismiss</button
				>{/if}
		</div>
		<progress
			class="progress progress-primary mt-3 w-full"
			value={job.completed_count}
			max={Math.max(1, job.expected_work_count)}
		></progress>
		<p class="mt-1 text-xs text-base-content/60">
			{job.completed_count.toLocaleString()} complete · {job.skipped_count.toLocaleString()} skipped ·
			{job.failed_count.toLocaleString()} failed
		</p>
	</div>
{/if}

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="bulk-review-title"
	onclose={() => opener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2 bind:this={dialogHeading} id="bulk-review-title" tabindex="-1" class="text-lg font-bold">
			{action === 'accept_candidate'
				? 'Accept one shared candidate'
				: `Preview bulk ${action.replace('_', ' ')}`}
		</h2>
		{#if preview.isPending}<div class="mt-4 skeleton h-28"></div>{:else if preview.isError}<div
				class="alert alert-error mt-4 text-sm"
			>
				Could not preview this selection. Nothing has been changed.
			</div>{:else if preview.data}<div class="mt-4 space-y-3 text-sm">
				{#if action === 'accept_candidate'}
					{#if (preview.data.common_candidate_keys ?? []).length}
						<label class="form-control w-full">
							<span class="label-text mb-1 font-medium"
								>Candidate available to every selected item</span
							>
							<select
								class="select select-bordered w-full font-mono text-xs"
								value={candidateKey ?? ''}
								onchange={(event) => void previewCandidate(event.currentTarget.value)}
							>
								<option value="">Choose a candidate</option>
								{#each preview.data.common_candidate_keys as key (key)}
									<option value={key}>{key}</option>
								{/each}
							</select>
						</label>
					{:else}
						<div class="alert alert-warning text-sm">
							No automatically safe candidate is shared by every selected item.
						</div>
					{/if}
				{/if}
				<p>
					<strong>{preview.data.eligible_count.toLocaleString()} eligible</strong> across {preview.data.album_count.toLocaleString()}
					albums and {preview.data.root_count.toLocaleString()} roots.
				</p>
				{#if preview.data.ineligible_count || preview.data.stale_count}<details
						class="rounded-box bg-base-200 p-3"
					>
						<summary class="cursor-pointer font-medium"
							>{(preview.data.ineligible_count + preview.data.stale_count).toLocaleString()} will not
							be changed</summary
						>
						<ul class="mt-2 space-y-1">
							{#each Object.entries(preview.data.reasons) as [reason, count] (reason)}<li>
									{count} · {reason.replaceAll('_', ' ').toLowerCase()}
								</li>{/each}{#if preview.data.stale_count}<li>
									{preview.data.stale_count} · changed since selection
								</li>{/if}
						</ul>
					</details>{/if}{#if preview.data.requires_local_metadata_confirmation}<p
						class="text-warning"
					>
						This is a one-off external identification action for Local metadata content. Its saved
						policy will not change.
					</p>{/if}{#if preview.data.crosses_policy_boundaries}<p class="text-warning">
						This selection crosses library policy boundaries.
					</p>{/if}
			</div>{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => dialog.close()}>Close</button><button
				class="btn btn-primary"
				disabled={!preview.data ||
					preview.data.eligible_count === 0 ||
					(action === 'accept_candidate' && !candidateKey) ||
					apply.isPending}
				onclick={() => void applyPreview()}
				>{#if apply.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Apply to {preview
					.data?.eligible_count ?? 0}</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close bulk preview">close</button>
	</form>
</dialog>
