<script lang="ts">
	import { AlertTriangle, CirclePause, CirclePlay, FolderTree, OctagonX } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import {
		getLibraryOperationQuery,
		getLibraryRunEstimateQuery
	} from '$lib/queries/library/LibraryOperationQueries.svelte';
	import { controlLibraryOperation } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import {
		applyBulkLibraryReview,
		previewBulkLibraryReview
	} from '$lib/queries/library/LibraryReviewMutations.svelte';
	import type { OperationResponse, ScanKind } from '$lib/queries/library/LibraryOperationsTypes';
	import { ApiError, TransportError } from '$lib/api/client';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		open: boolean;
		kind: Extract<ScanKind, 'rescan_files' | 'policy_reconcile'> | 'retry_identification';
		catalogRevision?: number;
		onconfirm: (scopeIds: string[]) => void | Promise<void>;
		onclose: () => void;
		pending?: boolean;
	}

	let { open, kind, catalogRevision = 0, onconfirm, onclose, pending = false }: Props = $props();
	let dialog: HTMLDialogElement;
	let dialogHeading: HTMLHeadingElement;
	let opener: HTMLElement | null = null;
	let selected = $state<string[]>([]);
	let activeJobId = $state<string | null>(null);
	const treeQuery = getLibraryPolicyTreeQuery(() => open);
	const estimateQuery = getLibraryRunEstimateQuery(
		() => selected,
		() => open && kind !== 'retry_identification' && treeQuery.isSuccess
	);
	const retryPreview = previewBulkLibraryReview();
	const retryApply = applyBulkLibraryReview();
	const operationQuery = getLibraryOperationQuery(() => activeJobId);
	const operationFinished = $derived(
		operationQuery.data !== undefined &&
			['succeeded', 'failed', 'cancelled', 'stopped'].includes(operationQuery.data.state)
	);
	const pause = controlLibraryOperation('pause');
	const resume = controlLibraryOperation('resume');
	const stop = controlLibraryOperation('stop');
	const retryError = $derived(retryApply.error ?? retryPreview.error);
	const retryIsStale = $derived(
		retryError instanceof ApiError && retryError.code === 'STALE_REVISION'
	);
	const retryIsTransport = $derived(retryError instanceof TransportError);

	const roots = $derived(treeQuery.data?.roots ?? []);
	const selectedNodes = $derived.by(() => {
		if (!selected.length) return roots.flatMap((root) => [root, ...root.children]);
		return roots.flatMap((root) => {
			if (selected.includes(root.id)) return [root, ...root.children];
			return root.children.filter((rule) => selected.includes(rule.id));
		});
	});
	const hasLocalMetadata = $derived(selectedNodes.some((node) => node.policy === 'local_metadata'));
	const hasExcluded = $derived(selectedNodes.some((node) => node.policy === 'excluded'));
	const hasUnavailable = $derived(selectedNodes.some((node) => !node.available));
	const selectionLabel = $derived(
		selected.length === 0 ? 'Whole library' : `${selected.length} scopes`
	);
	const estimatedFiles = $derived(estimateQuery.data?.estimated_file_count);
	const title = $derived(
		kind === 'rescan_files'
			? 'Rescan files'
			: kind === 'retry_identification'
				? 'Retry identification'
				: 'Apply policy changes'
	);
	const storageKey = $derived(
		`droppedneedle:identification-retry:${authStore.user?.id ?? 'anonymous'}`
	);
	const retrySelection = $derived({
		review_ids: [],
		expected_revisions: {},
		normalized_filter: {
			states: JSON.stringify(['needs_review', 'keep_tagged']),
			scope_revision: treeQuery.data?.policy_revision ?? '',
			...(selected.length ? { scope_ids: JSON.stringify([...selected].sort()) } : {})
		},
		catalog_revision: catalogRevision
	});

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) {
			opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			dialog.showModal();
			dialogHeading.focus();
		}
		if (!open && dialog.open) dialog.close();
	});

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

	function toggle(id: string): void {
		const node = roots
			.flatMap((root) => [root, ...root.children])
			.find((candidate) => candidate.id === id);
		if (node?.kind === 'rule') {
			selected = selected.includes(id) ? [] : [id];
		} else {
			const rootIds = new Set(roots.map((root) => root.id));
			const rootSelection = selected.filter((value) => rootIds.has(value));
			selected = rootSelection.includes(id)
				? rootSelection.filter((value) => value !== id)
				: [...rootSelection, id];
		}
		retryPreview.reset();
		retryApply.reset();
	}

	function selectWholeLibrary(): void {
		selected = [];
		retryPreview.reset();
		retryApply.reset();
	}

	function close(): void {
		selected = [];
		retryPreview.reset();
		retryApply.reset();
		onclose();
		opener?.focus();
	}

	async function previewRetry(): Promise<void> {
		try {
			await retryPreview.mutateAsync({ action: 'retry', selection: retrySelection });
		} catch {
			// mutation state keeps the error visible in this dialog
		}
	}

	async function startRetry(): Promise<void> {
		if (!retryPreview.data) return;
		let job: OperationResponse;
		try {
			job = await retryApply.mutateAsync({
				preview_token: retryPreview.data.preview_token,
				idempotency_key: createUuid(),
				action: 'retry',
				selection: retrySelection,
				confirm_local_metadata: retryPreview.data.requires_local_metadata_confirmation
			});
		} catch {
			return;
		}
		activeJobId = job.id;
		try {
			sessionStorage.setItem(storageKey, job.id);
		} catch {
			// The durable server job remains visible from Library operations.
		}
	}

	function startAnotherRetry(): void {
		activeJobId = null;
		retryPreview.reset();
		retryApply.reset();
	}
</script>

<dialog bind:this={dialog} class="modal" onclose={close} aria-labelledby="library-work-title">
	<div class="modal-box max-w-2xl">
		<h2
			bind:this={dialogHeading}
			id="library-work-title"
			tabindex="-1"
			class="flex items-center gap-2 text-xl font-bold"
		>
			<FolderTree class="h-5 w-5 text-primary" aria-hidden="true" />
			{title}
		</h2>
		<p class="mt-2 text-sm text-base-content/65">
			{kind === 'rescan_files'
				? 'Read file tags again for the selected library scopes. Existing indexed music stays available while the scan runs.'
				: kind === 'retry_identification'
					? 'Queue external identification for unresolved or Keep as tagged albums without rereading unchanged files.'
					: 'Reconcile the saved policies for the selected scopes. Work begins only after you confirm.'}
		</p>

		<div class="mt-5 space-y-2">
			<div class="flex items-center justify-between gap-3">
				<h3 class="font-semibold">Scope</h3>
				<button type="button" class="btn btn-ghost btn-xs" onclick={selectWholeLibrary}>
					Whole library
				</button>
			</div>
			{#if treeQuery.isLoading}
				<div class="skeleton h-28 rounded-box"></div>
			{:else if treeQuery.isError}
				<div class="alert alert-error text-sm">Could not load library scopes.</div>
			{:else}
				<div
					class="max-h-64 space-y-1 overflow-y-auto rounded-box border border-base-content/10 p-2"
				>
					{#each roots as root (root.id)}
						<label
							class="flex cursor-pointer items-center gap-3 rounded-lg px-2 py-2 hover:bg-base-200"
						>
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={selected.includes(root.id)}
								onchange={() => toggle(root.id)}
							/>
							<span class="min-w-0 flex-1">
								<span class="block truncate font-medium">{root.label}</span>
								<span class="block truncate text-xs text-base-content/55">{root.path}</span>
							</span>
							<span class="badge badge-outline badge-sm">{root.policy.replace('_', ' ')}</span>
							{#if !root.available}<span class="badge badge-warning badge-sm">Unavailable</span
								>{/if}
						</label>
						{#each root.children as rule (rule.id)}
							<label
								class="ml-6 flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-base-200"
							>
								<input
									type="checkbox"
									class="checkbox checkbox-sm"
									checked={selected.includes(rule.id)}
									onchange={() => toggle(rule.id)}
								/>
								<span class="min-w-0 flex-1 truncate text-sm">{rule.label}</span>
								<span class="badge badge-ghost badge-sm">{rule.policy.replace('_', ' ')}</span>
								{#if !rule.available}<span class="badge badge-warning badge-sm">Unavailable</span
									>{/if}
							</label>
						{/each}
					{/each}
				</div>
			{/if}
		</div>

		<div class="mt-4 rounded-box bg-base-200 p-3 text-sm">
			<div class="flex items-center justify-between gap-3">
				<span>{selectionLabel}</span>
				{#if kind === 'retry_identification' && retryPreview.isPending}
					<span class="skeleton h-4 w-28"></span>
				{:else if kind === 'retry_identification' && retryPreview.data}
					<strong>About {retryPreview.data.album_count.toLocaleString()} albums</strong>
				{:else if kind === 'retry_identification'}
					<span class="text-base-content/55">Preview to count albums</span>
				{:else if estimateQuery.isFetching}
					<span class="skeleton h-4 w-28"></span>
				{:else if estimatedFiles !== null && estimatedFiles !== undefined}
					<strong>About {estimatedFiles.toLocaleString()} files</strong>
				{:else}
					<span class="text-base-content/55">Count not available</span>
				{/if}
			</div>
			{#if estimateQuery.data?.estimated_at}
				<p class="mt-1 text-xs text-base-content/50">
					Approximate count from the latest inventory.
				</p>
			{/if}
		</div>

		{#if hasUnavailable}
			<div class="alert alert-warning mt-4 text-sm">
				<AlertTriangle class="h-4 w-4" /> An unavailable scope cannot be processed until it is mounted.
			</div>
		{/if}
		{#if kind === 'retry_identification' && hasLocalMetadata}
			<div class="alert alert-warning mt-4 text-sm">
				<AlertTriangle class="h-4 w-4" /> This is a one-off external identification action for Local metadata
				content. Its saved policy will not change.
			</div>
		{/if}
		{#if kind === 'retry_identification' && hasExcluded}
			<div class="alert alert-warning mt-4 text-sm">
				<AlertTriangle class="h-4 w-4" /> Excluded content is unavailable and will not be identified until
				it is restored.
			</div>
		{/if}
		{#if retryPreview.isError || retryApply.isError}
			<div class="alert alert-error mt-4 text-sm">
				{#if retryIsStale}
					<span>Library settings or review state changed. Reload the scopes and preview again.</span
					>
					<button
						class="btn btn-sm"
						onclick={() => {
							retryPreview.reset();
							retryApply.reset();
							void treeQuery.refetch();
						}}>Reload scopes</button
					>
				{:else if retryIsTransport}
					<span
						>DroppedNeedle could not reach the server, so the retry did not start. You can try again
						without closing this dialog.</span
					>
				{:else}
					<span>The retry could not start. No library data was changed.</span>
				{/if}
			</div>
		{/if}

		{#if kind === 'retry_identification' && operationQuery.data}
			{@const job = operationQuery.data}
			<div class="mt-4 rounded-box border border-primary/20 bg-base-100 p-4" role="status">
				<div class="flex flex-wrap items-center gap-2">
					<strong class="mr-auto capitalize">Identification retry {job.state}</strong>
					{#if job.state === 'running'}<button
							class="btn btn-ghost btn-sm"
							onclick={() =>
								void pause
									.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
									.catch(() => undefined)}><CirclePause class="h-4 w-4" /> Pause</button
						>{:else if job.state === 'paused'}<button
							class="btn btn-ghost btn-sm"
							onclick={() =>
								void resume
									.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
									.catch(() => undefined)}><CirclePlay class="h-4 w-4" /> Resume</button
						>{/if}
					{#if ['queued', 'running', 'paused'].includes(job.state)}<button
							class="btn btn-ghost btn-sm text-error"
							onclick={() =>
								void stop
									.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
									.catch(() => undefined)}><OctagonX class="h-4 w-4" /> Stop</button
						>{/if}
				</div>
				<progress
					class="progress progress-primary mt-3 w-full"
					value={job.completed_count}
					max={Math.max(1, job.expected_work_count)}
					aria-label="Identification retry progress"
				></progress>
				<p class="mt-1 text-xs text-base-content/60">
					{job.completed_count.toLocaleString()} complete · {job.skipped_count.toLocaleString()} skipped
					· {job.failed_count.toLocaleString()} failed
				</p>
			</div>
		{/if}

		<div class="modal-action">
			<button type="button" class="btn btn-ghost" onclick={() => dialog.close()}>Cancel</button>
			{#if kind === 'retry_identification' && operationFinished}
				<button type="button" class="btn btn-primary" onclick={startAnotherRetry}>
					Start another retry
				</button>
			{/if}
			{#if kind === 'retry_identification' && !operationQuery.data}
				{#if retryPreview.data}<button
						type="button"
						class="btn btn-primary"
						disabled={retryApply.isPending ||
							retryPreview.data.eligible_count === 0 ||
							hasUnavailable}
						onclick={() => void startRetry()}
						>{#if retryApply.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Retry
						{retryPreview.data.eligible_count.toLocaleString()} albums</button
					>{:else}<button
						type="button"
						class="btn btn-primary"
						disabled={retryPreview.isPending || treeQuery.isLoading || hasUnavailable}
						onclick={() => void previewRetry()}
						>{#if retryPreview.isPending}<span class="loading loading-spinner loading-sm"
							></span>{/if} Preview retry</button
					>{/if}
			{:else if kind !== 'retry_identification'}<button
					type="button"
					class="btn btn-primary"
					disabled={pending || treeQuery.isLoading}
					onclick={() => void onconfirm(selected)}
				>
					{#if pending}<span class="loading loading-spinner loading-sm"></span>{/if}
					{kind === 'rescan_files'
						? estimatedFiles
							? `Rescan about ${estimatedFiles.toLocaleString()} files`
							: 'Rescan selected files'
						: 'Apply policy changes'}
				</button>{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close dialog">close</button>
	</form>
</dialog>
