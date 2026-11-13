<script lang="ts">
	import { v3 } from '$lib/api/v3/endpoint';
	import '$lib/features/library-management/components/library-management.css';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import {
		TriangleAlert,
		CircleCheckBig,
		Clock3,
		History,
		RotateCcw,
		ServerCog,
		ShieldAlert
	} from 'lucide-svelte';

	import BackButton from '$lib/ui/BackButton.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		createControlLibraryManagementOperationMutation,
		createLibraryManagementUndoPreviewMutation
	} from '$lib/features/library-management/queries/LibraryManagementMutations.svelte';
	import {
		getLibraryManagementOperationQuery,
		getLibraryManagementOperationResultsQuery
	} from '$lib/features/library-management/queries/LibraryManagementQueries.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/features/library-management/queries/LibraryManagementPreviewTokens';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { createUuid } from '$lib/utils/uuid';
	import {
		managementAudioFormat,
		managementAlbumArtworkVersion,
		managementArtworkPreviewHash,
		managementDesiredField,
		managementPlanAlbum,
		managementPlanAlbumArtist,
		managementPlanArtist,
		managementPlanChanges,
		managementPlanTitle,
		managementPlanTrackLabel,
		managementReasonLabel,
		managementScopeCounts,
		titleManagementValue
	} from './LibraryManagementDisplay';
	import LibraryManagementAuditDossiers from './LibraryManagementAuditDossiers.svelte';
	import LibraryManagementResultInspector from './LibraryManagementResultInspector.svelte';
	import OperationJobStrip from './OperationJobStrip.svelte';
	import {
		groupManagementAuditEntries,
		type ManagementAuditEntry,
		type ManagementAuditTone
	} from './LibraryManagementAuditTypes';
	import type {
		LibraryManagementExternalRefresh,
		LibraryManagementResultItem,
		ManagementListScope
	} from '$lib/features/library-management/queries/types';

	interface Props {
		jobId: string;
	}

	let { jobId }: Props = $props();
	let stopOpen = $state(false);
	let undoOpen = $state(false);
	let nowSeconds = $state(Date.now() / 1000);
	let redirectingReadyPreview = $state(false);

	const operationQuery = getLibraryManagementOperationQuery(
		() => authStore.user?.id,
		() => jobId
	);
	const operation = $derived(operationQuery.data ?? null);
	let scope = $state<ManagementListScope>('changing');
	const scopeCounts = $derived(managementScopeCounts(operation?.summary));
	const resultsQuery = getLibraryManagementOperationResultsQuery(
		() => authStore.user?.id,
		() => jobId,
		() => 50,
		() => operation?.state ?? null,
		() => (scope === 'all' ? undefined : scope)
	);
	const pauseOperation = createControlLibraryManagementOperationMutation('pause');
	const resumeOperation = createControlLibraryManagementOperationMutation('resume');
	const stopOperation = createControlLibraryManagementOperationMutation('stop');
	const createUndo = createLibraryManagementUndoPreviewMutation();

	$effect(() => {
		if (!redirectingReadyPreview && operation?.mode === 'preview' && operation.state === 'ready') {
			redirectingReadyPreview = true;
			void goto(withBasePath(`/library/management/previews/${encodeURIComponent(jobId)}`), {
				replaceState: true
			});
		}
	});

	const externalRefreshes = $derived(operation?.external_refreshes ?? []);
	const results = $derived(resultsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const resultDossiers = $derived(
		groupManagementAuditEntries(results.map((item) => resultAuditEntry(item)))
	);
	const planningPreview = $derived(
		Boolean(
			operation &&
			operation.phase === 'planning' &&
			['queued', 'running', 'paused'].includes(operation.state)
		)
	);
	const plannedItems = $derived(operation?.summary.item_count ?? 0);
	const activePhaseLabel = $derived.by(() => {
		if (!operation) return 'Loading';
		const states = new Set(results.flatMap((item) => item.journal_states));
		if (states.has('cleanup_pending') || states.has('catalog_committed')) return 'Cleaning up';
		if (states.has('published')) return 'Committing catalog';
		if (states.has('source_backed_up') || states.has('validated')) return 'Publishing files';
		if (states.has('staged')) return 'Validating staged files';
		if (states.has('snapshot_saved')) return 'Writing staged files';
		if (states.has('planned')) return 'Preparing snapshots';
		return titleManagementValue(operation.phase);
	});
	const progress = $derived(
		operation && operation.expected_work_count > 0
			? Math.min(100, Math.round((operation.completed_count / operation.expected_work_count) * 100))
			: 0
	);
	const undoAvailable = $derived(
		Boolean(
			operation &&
			['succeeded', 'stopped'].includes(operation.state) &&
			operation.undo_available_count > 0 &&
			(operation.undo_expires_at == null || operation.undo_expires_at > nowSeconds)
		)
	);
	const undoStatus = $derived.by(() => {
		if (!operation) return '';
		if (operation.undo_expires_at != null && operation.undo_expires_at <= nowSeconds) {
			const count = operation.undo_available_count + operation.undo_expired_count;
			return `Undo snapshots have expired for ${count.toLocaleString()} ${count === 1 ? 'file' : 'files'}.`;
		}
		if (operation.undo_available_count > 0) {
			const count = operation.undo_available_count;
			const deadline = operation.undo_expires_at
				? ` The final snapshot expires ${formatDate(operation.undo_expires_at)}.`
				: '';
			return `${count.toLocaleString()} ${count === 1 ? 'file has' : 'files have'} an Undo snapshot.${deadline}`;
		}
		if (operation.undo_expired_count > 0) {
			return `Undo snapshots have expired for ${operation.undo_expired_count.toLocaleString()} ${operation.undo_expired_count === 1 ? 'file' : 'files'}.`;
		}
		return 'This operation has no completed file snapshots to undo.';
	});
	const baselineStatus = $derived.by(() => {
		if (!operation || operation.baseline_available_count <= 0) return '';
		const count = operation.baseline_available_count;
		return `${count.toLocaleString()} ${count === 1 ? 'file has' : 'files have'} an available original baseline.`;
	});
	const terminalPresentation = $derived.by(() => {
		if (!operation) return null;
		if (operation.state === 'failed') {
			return {
				className: 'alert-error',
				label: operation.terminal_code
					? titleManagementValue(operation.terminal_code)
					: 'Operation Failed',
				detail:
					'Inspect the per-file results below. Recovery never silently removes an uncertain file.'
			};
		}
		if (!operation.terminal_code) return null;
		if (operation.terminal_code === 'COMPLETED_WITH_ERRORS') {
			return {
				className: 'alert-warning',
				label: titleManagementValue(operation.terminal_code),
				detail: 'The operation finished, but some files failed. Inspect their results below.'
			};
		}
		if (operation.terminal_code === 'COMPLETED_WITH_SKIPS') {
			return {
				className: 'alert-info',
				label: titleManagementValue(operation.terminal_code),
				detail: 'The operation finished with skipped files. Inspect their reasons below.'
			};
		}
		if (
			operation.terminal_code === 'FILES_CHANGED_OUTSIDE' ||
			operation.terminal_code === 'TAKEN_OVER'
		) {
			return {
				className: 'alert-info',
				label: managementReasonLabel(operation.terminal_code),
				detail:
					'The album is left unmanaged. Choose "Manage it again" on its page to have DroppedNeedle retag and rename it from its edition.'
			};
		}
		if (operation.terminal_code === 'ACTIVATION_CONFIRMED') {
			return {
				className: 'alert-success',
				label: 'Dry run confirmed',
				detail:
					'This dry run turned on automatic management for its library folder. It changed no files itself.'
			};
		}
		if (operation.state === 'succeeded') {
			return {
				className: 'alert-success',
				label: titleManagementValue(operation.terminal_code),
				detail: 'All planned work finished.'
			};
		}
		return {
			className: 'alert-info',
			label: titleManagementValue(operation.terminal_code),
			detail: 'Inspect the per-file results below for the recorded outcome.'
		};
	});

	onMount(() => {
		const timer = window.setInterval(() => {
			nowSeconds = Date.now() / 1000;
		}, 30_000);
		return () => window.clearInterval(timer);
	});

	async function control(action: 'pause' | 'resume'): Promise<void> {
		if (!operation) return;
		const mutation = action === 'pause' ? pauseOperation : resumeOperation;
		await mutation
			.mutateAsync({ jobId, expectedRevision: operation.operation_row_revision })
			.catch(() => undefined);
	}

	async function stop(): Promise<void> {
		if (!operation) return;
		await stopOperation.mutateAsync({
			jobId,
			expectedRevision: operation.operation_row_revision
		});
	}

	async function previewUndo(): Promise<void> {
		if (!operation || !undoAvailable) return;
		const handle = await createUndo.mutateAsync({
			jobId,
			request: {
				expected_operation_row_revision: operation.operation_row_revision,
				idempotency_key: createUuid()
			}
		});
		rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
		await goto(withBasePath(`/library/management/previews/${encodeURIComponent(handle.job_id)}`));
	}

	function formatDate(value: number): string {
		return new Date(value * 1000).toLocaleString();
	}

	function refreshStatusCopy(refresh: LibraryManagementExternalRefresh): string {
		if (refresh.state === 'done') return 'Rescan asked for.';
		if (refresh.state === 'running') return 'Asking now.';
		if (refresh.state === 'failed') return 'Gave up.';
		if (refresh.state === 'no_record') return 'No record kept.';
		return refresh.code ? 'Will try again.' : 'Waiting to ask.';
	}

	function refreshBadge(refresh: LibraryManagementExternalRefresh): string {
		if (refresh.state === 'done') return 'badge-success';
		if (refresh.state === 'failed') return 'badge-error';
		if (refresh.state === 'queued' && refresh.code) return 'badge-warning';
		return 'badge-outline';
	}

	function resultTone(item: LibraryManagementResultItem): ManagementAuditTone {
		if (item.failure_code || item.work_state === 'failed') return 'error';
		if (item.work_state === 'skipped') return 'warning';
		if (item.work_state === 'succeeded') return 'success';
		return 'neutral';
	}

	function resultAuditEntry(item: LibraryManagementResultItem): ManagementAuditEntry {
		const artworkHash = (Array.isArray(item.plan.artwork_choices) ? item.plan.artwork_choices : [])
			.map(managementArtworkPreviewHash)
			.find((value): value is string => Boolean(value));
		return {
			ordinal: item.plan.ordinal,
			bundleOrdinal: item.plan.bundle_ordinal,
			trackLabel: managementPlanTrackLabel(item.plan),
			title: managementPlanTitle(item.plan),
			artist: managementPlanArtist(item.plan),
			albumTitle: managementPlanAlbum(item.plan),
			albumArtist: managementPlanAlbumArtist(item.plan),
			albumId: item.plan.local_album_id ?? null,
			albumMbid: managementDesiredField(item.plan, 'musicbrainz_release_group_id'),
			albumArtworkVersion: managementAlbumArtworkVersion(item.plan),
			format: managementAudioFormat(item.plan),
			status: item.failure_code
				? managementReasonLabel(item.failure_code)
				: titleManagementValue(item.work_state),
			statusTone: resultTone(item),
			reason: item.failure_code ? managementReasonLabel(item.failure_code) : null,
			reasonCode: item.failure_code ?? null,
			changes: managementPlanChanges(item.plan),
			exceptional: Boolean(item.failure_code || item.work_state !== 'succeeded'),
			sourceRoot: 'Source',
			sourcePath: item.plan.source_relative_path ?? null,
			destinationRoot: 'Destination',
			destinationPath: item.plan.destination_relative_path ?? null,
			artworkUrl: artworkHash
				? v3('/api/v3/library/management/previews/{job_id}/items/{ordinal}/artwork/{sha256}', {
						path: { job_id: jobId, ordinal: item.plan.ordinal, sha256: artworkHash }
					})
				: null
		};
	}
</script>

<svelte:head><title>Organization operation - DroppedNeedle</title></svelte:head>

{#snippet resultsState()}
	{#if resultsQuery.isLoading}<div
			class="space-y-2"
			role="status"
			aria-label="Loading file results"
		>
			<div class="skeleton h-20"></div>
			<div class="skeleton h-20"></div>
		</div>{:else if resultsQuery.isError}<div class="alert alert-error" role="alert">
			Could not load operation results.
		</div>{:else if scope === 'changing' && plannedItems > 0}<div
			class="rounded-xl border border-dashed border-base-content/15 p-5 text-sm text-muted"
		>
			Nothing in this operation changes a file.
			<div class="mt-2">
				<Button variant="ghost" size="sm" onclick={() => (scope = 'all')}>Show all files</Button>
			</div>
		</div>{:else}<div
			class="rounded-xl border border-dashed border-base-content/15 p-5 text-sm text-muted"
		>
			No per-file results have been recorded yet.
		</div>{/if}
{/snippet}

{#snippet resultInspector(ordinal: number)}
	{@const result = results.find((candidate) => candidate.plan.ordinal === ordinal)}
	{#if result}<LibraryManagementResultInspector
			item={result}
			operationState={operation?.state}
		/>{/if}
{/snippet}

<div class="management-preview-shell px-4 py-8 sm:px-6 lg:px-8">
	<main class="mx-auto max-w-7xl space-y-5">
		<BackButton fallback={withBasePath('/library/management?tab=organize')} />

		{#if operationQuery.isLoading || redirectingReadyPreview}
			<div class="space-y-4" role="status" aria-label="Loading organization operation">
				<div class="skeleton h-48 rounded-2xl"></div>
				<div class="skeleton h-72 rounded-2xl"></div>
			</div>
		{:else if operationQuery.isError}
			<div class="alert alert-error" role="alert">Could not load this Organization operation.</div>
		{:else if operation}
			<header class="op-head">
				<div class="flex flex-wrap items-start gap-4">
					<div class="min-w-0 flex-1">
						<p class="management-kicker">
							<ShieldAlert class="h-3.5 w-3.5" /> File-writing operation
						</p>
						<h1 class="lm-page-title mt-2">
							{titleManagementValue(operation.mode)}
						</h1>
						<p class="mt-3 text-muted">
							{operation.profile_name} · {titleManagementValue(operation.origin)} · started {formatDate(
								operation.created_at
							)}
						</p>
					</div>
					<StatusChip
						size="md"
						tone={operation.state === 'failed'
							? 'error'
							: operation.state === 'succeeded'
								? 'success'
								: operation.state === 'paused'
									? 'warning'
									: 'neutral'}>{titleManagementValue(operation.state)}</StatusChip
					>
				</div>
				<div class="lm-bordered op-progress mt-6">
					<span class="sr-only" role="status" aria-live="polite"
						>Operation phase: {activePhaseLabel}</span
					>
					<div class="mb-2 flex justify-between gap-3 text-xs">
						<span>{activePhaseLabel}</span><span>
							{planningPreview
								? plannedItems > 0
									? `${plannedItems.toLocaleString()} files planned so far`
									: 'Finding files and albums'
								: `${operation.completed_count.toLocaleString()} / ${operation.expected_work_count.toLocaleString()}`}
						</span>
					</div>
					{#if planningPreview}
						<progress
							class="progress progress-warning w-full"
							max="100"
							aria-label={`Planning preview; ${plannedItems.toLocaleString()} files planned so far`}
						></progress>
					{:else}
						<progress
							class="progress progress-warning w-full"
							value={progress}
							max="100"
							aria-label={`${progress}% complete`}
						></progress>
					{/if}
				</div>
				<div class="mt-4 grid gap-2 sm:grid-cols-4">
					<div class="management-summary-card">
						<span class="text-xs text-muted">Succeeded</span><strong
							>{operation.succeeded_count}</strong
						>
					</div>
					<div class="management-summary-card">
						<span class="text-xs text-muted">Failed</span><strong>{operation.failed_count}</strong>
					</div>
					<div class="management-summary-card">
						<span class="text-xs text-muted">Skipped</span><strong>{operation.skipped_count}</strong
						>
					</div>
					<div class="management-summary-card">
						<span class="text-xs text-muted">Revision</span><strong
							>{operation.operation_row_revision}</strong
						>
					</div>
				</div>
				<OperationJobStrip
					class="mt-4"
					status={operation.state}
					onpause={() => control('pause')}
					onresume={() => control('resume')}
					onstop={() => (stopOpen = true)}
					confirmStop
				/>
			</header>

			{#if terminalPresentation}<div
					class={`alert ${terminalPresentation.className}`}
					role={operation.state === 'failed' ? 'alert' : 'status'}
				>
					{#if operation.state === 'succeeded'}<CircleCheckBig
							class="h-5 w-5"
						/>{:else}<TriangleAlert class="h-5 w-5" />{/if}<span
						><strong>{terminalPresentation.label}</strong><br />{terminalPresentation.detail}</span
					>
				</div>{/if}

			{#if externalRefreshes.length}<Card as="section" variant="bordered" padding="md">
					<div class="flex items-start gap-3">
						<div class="mt-0.5 rounded-lg border border-base-content/10 bg-base-200/60 p-2">
							<ServerCog class="h-4 w-4 text-library-manage" />
						</div>
						<div>
							<p class="management-step">After the run</p>
							<h2 class="font-display text-xl font-semibold">Media server rescans</h2>
							<p class="mt-1 text-sm text-muted">A failed rescan never undoes the run's changes.</p>
						</div>
					</div>
					<div class="mt-4 divide-y divide-base-content/10 border-y border-base-content/10">
						{#each externalRefreshes as refresh (refresh.target)}<div
								class="grid gap-2 py-3 sm:grid-cols-[10rem_1fr_auto] sm:items-center"
							>
								<strong>{titleManagementValue(refresh.target)}</strong>
								<div>
									<p class="text-sm text-muted">{refreshStatusCopy(refresh)}</p>
									{#if refresh.attempts || refresh.code}<p class="mt-0.5 text-xs text-muted">
											{refresh.attempts}
											{refresh.attempts === 1 ? 'try' : 'tries'}{#if refresh.code}
												· {managementReasonLabel(refresh.code)}{/if}
										</p>{/if}
								</div>
								<span class="badge badge-sm {refreshBadge(refresh)}"
									>{titleManagementValue(refresh.state)}</span
								>
							</div>{/each}
					</div>
				</Card>{/if}

			<Card as="section" variant="bordered" padding="md">
				<div class="flex flex-wrap items-end justify-between gap-2">
					<div>
						<p class="management-step">Audit trail</p>
						<h2 class="font-display text-xl font-semibold">Per-file results</h2>
					</div>
					<Button
						variant="ghost"
						size="sm"
						icon={History}
						href={withBasePath('/library/management/history')}>All history</Button
					>
				</div>
				<LibraryManagementAuditDossiers
					dossiers={resultDossiers}
					inspector={resultInspector}
					bind:scope
					{scopeCounts}
					placeholder={resultsQuery.isLoading || resultsQuery.isError || results.length === 0
						? resultsState
						: undefined}
					detailLabel="Inspect result evidence"
					incomplete={Boolean(resultsQuery.hasNextPage)}
				/>
				{#if results.length > 0 && resultsQuery.hasNextPage}<Button
						variant="outline"
						class="w-full"
						loading={resultsQuery.isFetchingNextPage}
						onclick={() => void resultsQuery.fetchNextPage()}>Load more results</Button
					>{/if}
			</Card>

			<section class="grid gap-3 sm:grid-cols-2">
				<Card variant="bordered" padding="md">
					<RotateCcw class="h-5 w-5 text-library-manage" />
					<h2 class="mt-2 font-semibold">Undo this operation</h2>
					<p class="mt-1 text-sm text-muted">
						Creates a separate preview from this operation's before-state snapshots. Files changed
						again later are skipped.
					</p>
					<p class="mt-2 text-xs text-muted">{undoStatus}</p>
					<Button
						variant="outline"
						size="sm"
						class="mt-3 self-start"
						disabled={!undoAvailable}
						onclick={() => (undoOpen = true)}>Preview Undo...</Button
					>{#if operation.undo_available_count > 0 && !undoAvailable && (operation.undo_expires_at == null || operation.undo_expires_at > nowSeconds)}<p
							class="mt-2 text-xs text-muted"
						>
							Undo becomes available when this operation reaches a successful or stopped state.
						</p>{/if}
				</Card>
				<Card variant="bordered" padding="md">
					<Clock3 class="h-5 w-5 text-muted" />
					<h2 class="mt-2 font-semibold">Original baseline</h2>
					<p class="mt-1 text-sm text-muted">
						Restore files to how they were before DroppedNeedle first managed them. This is broader
						than Undo and leaves restored files unmanaged.
					</p>
					{#if operation.baseline_available_count > 0}<p class="mt-2 text-xs text-muted">
							{baselineStatus}
						</p>{/if}
					<Button
						variant="ghost"
						size="sm"
						class="mt-3 self-start"
						href={withBasePath('/library/management?tab=organize&runner=baseline_restore')}
						>Open baseline restore...</Button
					>
				</Card>
			</section>
		{/if}
	</main>
</div>

<ConfirmDialog
	bind:open={stopOpen}
	title="Stop after the current safe boundary?"
	message="Stopping keeps completed changes. It does not roll them back. Use Undo after the job finishes or stops."
	confirmLabel="Stop operation"
	cancelLabel="Keep working"
	tone="danger"
	focusTitle
	pending={stopOperation.isPending}
	onconfirm={stop}
/>

<ConfirmDialog
	bind:open={undoOpen}
	title="Generate an Undo preview?"
	message="This does not immediately change files. A full preview will show restorable, changed, expired, and blocked items before a separate Apply confirmation."
	confirmLabel="Generate Undo preview"
	tone="warning"
	icon={RotateCcw}
	focusTitle
	pending={createUndo.isPending}
	canConfirm={undoAvailable}
	onconfirm={previewUndo}
>
	{#if operation}<p class="mt-2 text-xs text-muted">{undoStatus}</p>{/if}
	<div class="alert alert-info mt-3 text-sm">
		<span
			><strong>Undo is not baseline restore.</strong> It targets only this operation's completed changes.</span
		>
	</div>
</ConfirmDialog>

<style>
	.op-progress {
		padding: 1rem 1.25rem;
		background-color: var(--color-base-200);
	}
	.op-progress > div:first-of-type {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}
</style>
