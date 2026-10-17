<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import { onMount } from 'svelte';
	import { SvelteURL } from 'svelte/reactivity';
	import {
		TriangleAlert,
		ArrowRight,
		History,
		RotateCcw,
		Settings2,
		Sparkles
	} from 'lucide-svelte';

	import LibraryManagementRunner from './LibraryManagementRunner.svelte';
	import LibraryManagementDiscardPreview from './LibraryManagementDiscardPreview.svelte';
	import LibraryManagementIdentityReadiness from './LibraryManagementIdentityReadiness.svelte';
	import LibraryRepairPanel from './LibraryRepairPanel.svelte';
	import OperationJobStrip from './OperationJobStrip.svelte';
	import RetagPairCard from './RetagPairCard.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { invalidateLibraryManagementSurfaces } from '$lib/queries/library-management/LibraryManagementInvalidation';
	import {
		STALE_INPUT_HINT,
		groupFailedOperationsByAlbum,
		isStaleInputTerminal
	} from './LibraryWorkPresentation';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { createLibraryManagementEvents } from '$lib/queries/library-management/LibraryManagementEvents';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		controlLibraryManagementOperationMutation,
		discardLibraryManagementPreviewMutation,
		handBackAllLibraryAlbumsMutation,
		handBackLibraryAlbumMutation,
		handBackRefusalMessage,
		reissueLibraryManagementPreviewMutation,
		resolveLibraryManagementImportBundleMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import {
		forgetLibraryManagementPreviewToken,
		rememberLibraryManagementPreviewToken
	} from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import {
		getLibraryManagementOperationsQuery,
		getLibraryManagementAutomaticSkippedQuery,
		getLibraryManagementRecoveryQuery,
		getLibraryManagementSettingsQuery,
		getLibraryManagementTakeoversQuery,
		TAKEOVERS_POLL_MS
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import { getRetagPairsQuery } from '$lib/queries/library/LibraryRetagPairQueries.svelte';

	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const policyQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const operationsQuery = getLibraryManagementOperationsQuery(
		() => authStore.user?.id,
		() => ({ limit: 20 })
	);
	const recoveryQuery = getLibraryManagementRecoveryQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const pauseOperation = controlLibraryManagementOperationMutation('pause');
	const resumeOperation = controlLibraryManagementOperationMutation('resume');
	const reissuePreview = reissueLibraryManagementPreviewMutation();
	const discardPreview = discardLibraryManagementPreviewMutation();
	const resolveImportBundle = resolveLibraryManagementImportBundleMutation();
	let resolvingBundleId = $state<string | null>(null);
	type RunnerMode = 'manage' | 'baseline_restore';

	let runnerMode = $state<RunnerMode | null>(runnerModeFromUrl());
	let runnerOpener = $state<HTMLElement | null>(null);

	const history = $derived(operationsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const active = $derived(
		history.find((item) => ['queued', 'running', 'paused'].includes(item.operation.state)) ?? null
	);
	const activeProgressLabel = $derived.by(() => {
		if (!active) return '';
		if (active.operation.expected_work_count > 0) {
			return `${active.operation.completed_count.toLocaleString()} / ${active.operation.expected_work_count.toLocaleString()}`;
		}
		return active.phase === 'planning'
			? 'Discovering files and release bundles'
			: 'Preparing the preview';
	});
	const readyPreviews = $derived(
		history
			.filter((item) => item.operation.state === 'ready' && !item.activation_preview)
			.slice(0, 3)
	);
	const recent = $derived(history.filter((item) => item.operation.state !== 'ready').slice(0, 5));
	const recentRunning = $derived(recent.filter((item) => item.operation.state !== 'failed'));
	const recentFailed = $derived(recent.filter((item) => item.operation.state === 'failed'));
	const failedGroups = $derived(
		groupFailedOperationsByAlbum(recentFailed, (item) => isStaleInputTerminal(item.operation))
	);
	let bulkPending = $state(false);
	let bulkSummary = $state<string | null>(null);
	const activeAssignments = $derived(
		(settingsQuery.data?.root_assignments ?? []).filter(
			(assignment) =>
				assignment.enabled &&
				(assignment.automatic_acquisitions ||
					assignment.automatic_drop_imports ||
					assignment.automatic_scan_discovered)
		)
	);
	const needsAttentionBundles = $derived(recoveryQuery.data?.needs_attention_bundles ?? []);
	const retryingBundles = $derived(recoveryQuery.data?.retrying_bundles ?? []);
	const leftoverFiles = $derived(recoveryQuery.data?.leftover_files ?? []);
	const leftoverFileCount = $derived(recoveryQuery.data?.leftover_file_count ?? 0);
	const attentionCount = $derived(
		(recoveryQuery.data?.needs_attention_count ?? 0) +
			(recoveryQuery.data?.cleanup_pending_count ?? 0) +
			retryingBundles.length +
			leftoverFileCount +
			history.filter(
				(item) => item.operation.state === 'failed' && !isStaleInputTerminal(item.operation)
			).length
	);
	const recoveryUnavailable = $derived(recoveryQuery.isError);
	const takeoversQuery = getLibraryManagementTakeoversQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const retagPairsQuery = getRetagPairsQuery(() => authStore.isAdmin);
	const handingBack = $derived(takeoversQuery.data?.handing_back ?? 0);
	const skippedQuery = getLibraryManagementAutomaticSkippedQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin,
		() => (handingBack > 0 ? TAKEOVERS_POLL_MS : false)
	);
	const skippedAlbums = $derived(skippedQuery.data?.items ?? []);
	const handBackAlbum = handBackLibraryAlbumMutation();
	const handBackAll = handBackAllLibraryAlbumsMutation();
	// Albums whose files could not be read for a hand-back, with the reason.
	let handBackRefusals = $state<Record<string, string>>({});

	async function handBackOne(albumId: string): Promise<void> {
		try {
			await handBackAlbum.mutateAsync({ albumId });
			delete handBackRefusals[albumId];
		} catch (error) {
			const refusal = handBackRefusalMessage(error);
			if (refusal) handBackRefusals[albumId] = refusal;
		}
	}

	function day(millis: number): string {
		return new Date(millis).toLocaleDateString(undefined, { dateStyle: 'medium' });
	}

	onMount(() => {
		const events = createLibraryManagementEvents();
		events.start();
		return events.stop;
	});

	function runnerModeFromUrl(): RunnerMode | null {
		const requested = page.url.searchParams.get('runner');
		return requested === 'manage' || requested === 'baseline_restore' ? requested : null;
	}

	function updateRunnerUrl(mode: RunnerMode | null): void {
		const url = new SvelteURL(page.url);
		if (mode) url.searchParams.set('runner', mode);
		else url.searchParams.delete('runner');
		url.hash = 'management-controls';
		replaceState(url, page.state);
	}

	function openRunner(mode: RunnerMode, opener: HTMLElement): void {
		runnerOpener = opener;
		runnerMode = mode;
		updateRunnerUrl(mode);
	}

	function closeRunner(): void {
		runnerMode = null;
		updateRunnerUrl(null);
		runnerOpener?.focus();
		runnerOpener = null;
	}

	function title(value: string): string {
		return value.replaceAll('_', ' ').replace(/^\w/, (letter) => letter.toUpperCase());
	}

	function operationHref(
		jobId: string,
		state: string,
		terminalCode: string | null,
		mode: string
	): string {
		return mode === 'preview' || state === 'ready' || terminalCode === 'PREVIEW_DISCARDED'
			? withBasePath(`/library/management/previews/${encodeURIComponent(jobId)}`)
			: withBasePath(`/library/management/operations/${encodeURIComponent(jobId)}`);
	}

	function date(value: number): string {
		return new Date(value * 1000).toLocaleString();
	}

	type StaleGroupItems = Array<(typeof failedGroups)[number]['items'][number]>;

	function staleMembers(items: StaleGroupItems): StaleGroupItems {
		return items.filter((item) => isStaleInputTerminal(item.operation));
	}

	async function bulkRetryStale(items: StaleGroupItems): Promise<void> {
		const targets = staleMembers(items);
		if (!authStore.isAdmin || bulkPending || targets.length === 0) return;
		bulkPending = true;
		bulkSummary = null;
		let succeeded = 0;
		let failed = 0;
		for (const item of targets) {
			try {
				const handle = await reissuePreview.mutateAsync({ jobId: item.operation.id, silent: true });
				rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
				succeeded += 1;
			} catch {
				failed += 1;
			}
		}
		await invalidateLibraryManagementSurfaces().catch(() => undefined);
		bulkPending = false;
		const summary =
			`Retried ${succeeded} of ${targets.length} stale ${targets.length === 1 ? 'preview' : 'previews'}` +
			(failed ? ` (${failed} failed)` : '') +
			'.';
		bulkSummary = summary;
		toastStore.show({ message: summary, type: failed ? 'error' : 'success' });
	}

	async function resolveNeedsAttentionBundle(bundleId: string): Promise<void> {
		if (!authStore.isAdmin || resolvingBundleId) return;
		resolvingBundleId = bundleId;
		try {
			const result = await resolveImportBundle.mutateAsync({ bundleId });
			await invalidateLibraryManagementSurfaces().catch(() => undefined);
			toastStore.show({
				message: result.note,
				type: 'success'
			});
		} catch (error) {
			toastStore.show({
				message:
					error instanceof Error && error.message
						? error.message
						: 'Could not mark this write as handled',
				type: 'error'
			});
		} finally {
			resolvingBundleId = null;
		}
	}

	async function bulkDismissStale(items: StaleGroupItems): Promise<void> {
		const targets = staleMembers(items);
		if (!authStore.isAdmin || bulkPending || targets.length === 0) return;
		bulkPending = true;
		bulkSummary = null;
		let succeeded = 0;
		let failed = 0;
		for (const item of targets) {
			try {
				await discardPreview.mutateAsync({
					jobId: item.operation.id,
					request: { expected_operation_row_revision: item.operation.row_revision },
					silent: true
				});
				forgetLibraryManagementPreviewToken(item.operation.id);
				succeeded += 1;
			} catch {
				failed += 1;
			}
		}
		await invalidateLibraryManagementSurfaces().catch(() => undefined);
		bulkPending = false;
		const summary =
			`Dismissed ${succeeded} of ${targets.length} stale ${targets.length === 1 ? 'preview' : 'previews'}` +
			(failed ? ` (${failed} failed)` : '') +
			'.';
		bulkSummary = summary;
		toastStore.show({ message: summary, type: failed ? 'error' : 'success' });
	}
</script>

<section id="management-controls" class="space-y-5" aria-label="Organize files">
	<div
		class="rounded-3xl border border-warning/20 bg-gradient-to-br from-warning/8 via-base-200/50 to-base-200/40 p-5 sm:p-6"
	>
		<p class="management-kicker">Optional write access</p>
		<div class="mt-2 flex flex-wrap items-center justify-between gap-4">
			<div class="max-w-2xl">
				<h2 class="font-display text-2xl font-bold">Organize files</h2>
				<p class="mt-1 text-sm text-base-content/60">
					Writes tags and organizes files on disk - nothing changes until you review and apply a
					preview.
				</p>
			</div>
			<div class="flex flex-wrap items-center gap-2">
				<Button
					variant="warning"
					icon={Sparkles}
					disabled={recoveryUnavailable || !settingsQuery.data || !policyQuery.data}
					onclick={(event) => openRunner('manage', event.currentTarget)}
					>Preview organization...</Button
				>
				<Button
					variant="outline"
					icon={RotateCcw}
					disabled={recoveryUnavailable || !settingsQuery.data || !policyQuery.data}
					onclick={(event) => openRunner('baseline_restore', event.currentTarget)}
					>Restore original state...</Button
				>
				<Button
					variant="ghost"
					size="sm"
					icon={Settings2}
					href={withBasePath('/library/management?tab=automation')}>Automation</Button
				>
			</div>
		</div>
	</div>

	{#if settingsQuery.isLoading || policyQuery.isLoading || operationsQuery.isLoading || recoveryQuery.isLoading}
		<div class="space-y-3" role="status" aria-label="Loading file-organization state">
			<div class="skeleton h-28 rounded-xl"></div>
			<div class="skeleton h-44 rounded-xl"></div>
		</div>
	{:else if settingsQuery.isError || policyQuery.isError || operationsQuery.isError}
		<div class="alert alert-error" role="alert">Could not load file-organization state.</div>
	{:else if settingsQuery.data && policyQuery.data}
		<div class="space-y-5">
			<div class="grid gap-3 sm:grid-cols-3">
				<div class="management-control-stat">
					<span>Automatic write access</span><strong
						>{activeAssignments.length
							? `${activeAssignments.length} active root${activeAssignments.length === 1 ? '' : 's'}`
							: 'Off everywhere'}</strong
					><small>Manual previews remain available whether automation is on or off.</small>
				</div>
				<div class="management-control-stat">
					<span>Ready previews</span><strong>{readyPreviews.length}</strong><small
						>Nothing writes until an administrator opens and applies one.</small
					>
				</div>
				<div
					class="management-control-stat"
					data-attention={attentionCount > 0 || recoveryUnavailable}
				>
					<span>Needs attention</span><strong
						>{recoveryUnavailable ? 'Status unavailable' : attentionCount}</strong
					><small
						>{recoveryUnavailable
							? 'Recovery diagnostics could not be loaded.'
							: `${recoveryQuery.data?.nonterminal_journal_count ?? 0} nonterminal recovery journals.`}</small
					>
				</div>
			</div>

			<LibraryManagementIdentityReadiness roots={policyQuery.data.library_roots} />

			<LibraryRepairPanel />

			{#if active}
				<article class="management-active-card">
					<div class="flex min-w-0 flex-1 items-start gap-3">
						<span class="management-live-dot" aria-hidden="true"></span>
						<div class="min-w-0">
							<p class="management-step">
								{active.activation_preview ? 'Write-access dry run' : 'Active write work'}
							</p>
							<h3 class="font-semibold">{active.profile_name}</h3>
							<p class="text-sm text-base-content/55">
								{title(active.mode)} · {title(active.phase)} · {activeProgressLabel}
							</p>
						</div>
					</div>
					<OperationJobStrip
						status={active.operation.state}
						onpause={() =>
							pauseOperation
								.mutateAsync({
									jobId: active.operation.id,
									expectedRevision: active.operation.row_revision
								})
								.catch(() => undefined)}
						onresume={() =>
							resumeOperation
								.mutateAsync({
									jobId: active.operation.id,
									expectedRevision: active.operation.row_revision
								})
								.catch(() => undefined)}
					>
						<Button
							variant="ghost"
							size="sm"
							icon={ArrowRight}
							iconPosition="end"
							href={operationHref(
								active.operation.id,
								active.operation.state,
								active.operation.terminal_code,
								active.mode
							)}>Open details</Button
						>
					</OperationJobStrip>
				</article>
			{/if}

			{#if readyPreviews.length}
				<section class="space-y-2" aria-labelledby="ready-management-previews">
					<div class="flex items-end justify-between">
						<div>
							<p class="management-step">Awaiting review</p>
							<h3 id="ready-management-previews" class="font-semibold">Ready previews</h3>
						</div>
						<span class="text-xs text-base-content/45">Applying is the first write action</span>
					</div>
					{#each readyPreviews as item (item.operation.id)}<div class="flex items-stretch gap-1">
							<a
								href={operationHref(
									item.operation.id,
									item.operation.state,
									item.operation.terminal_code,
									item.mode
								)}
								class="management-history-row min-w-0 flex-1"
								><Sparkles class="h-4 w-4 shrink-0 text-library-manage" /><span
									class="min-w-0 flex-1"
									><strong>{item.profile_name}</strong><small
										>{title(item.mode)} · {date(item.operation.updated_at)}</small
									><span class="mt-1 flex flex-wrap gap-1"
										>{#if (item.eligible_count ?? 0) > 0}<span class="badge badge-success badge-sm"
												>{item.eligible_count} eligible</span
											>{/if}{#if (item.warning_count ?? 0) > 0}<span
												class="badge badge-warning badge-sm">{item.warning_count} warning</span
											>{/if}{#if (item.blocked_count ?? 0) > 0}<span
												class="badge badge-error badge-sm">{item.blocked_count} blocked</span
											>{/if}</span
									></span
								><span class="badge badge-outline badge-sm">Review</span><ArrowRight
									class="h-4 w-4 shrink-0"
								/></a
							>
							<LibraryManagementDiscardPreview
								jobId={item.operation.id}
								expectedRevision={item.operation.row_revision}
								profileName={item.profile_name}
								compact
							/>
						</div>{/each}
				</section>
			{/if}

			<section class="space-y-2" aria-labelledby="recent-management-work">
				<div class="flex items-end justify-between">
					<div>
						<p class="management-step">Audit trail</p>
						<h3 id="recent-management-work" class="font-semibold">Recent management work</h3>
					</div>
					{#if recent.length > 0}
						{#each recentRunning as item (item.operation.id)}<a
								href={operationHref(
									item.operation.id,
									item.operation.state,
									item.operation.terminal_code,
									item.mode
								)}
								class="management-history-row"
								><History class="h-4 w-4 text-base-content/45" /><span class="min-w-0 flex-1"
									><strong>{item.profile_name}</strong><small
										>{title(item.mode)} · {title(item.operation.state)} · {date(
											item.operation.updated_at
										)}</small
									><span class="mt-1 flex flex-wrap gap-1"
										>{#if item.operation.succeeded_count}<span class="badge badge-success badge-sm"
												>{item.operation.succeeded_count} succeeded</span
											>{/if}{#if item.operation.failed_count}<span
												class="badge badge-error badge-sm"
												>{item.operation.failed_count} failed</span
											>{/if}{#if item.operation.skipped_count}<span
												class="badge badge-warning badge-sm"
												>{item.operation.skipped_count} skipped</span
											>{/if}</span
									></span
								><ArrowRight class="h-4 w-4" /></a
							>{/each}
						{#each failedGroups as group (group.key)}
							{#if group.items.length > 1}
								<article
									class="management-history-row"
									aria-label={`${group.items[0].profile_name}: ${group.items.length} failed attempts for the same album`}
								>
									<History class="h-4 w-4 shrink-0 text-base-content/45" />
									<div class="min-w-0 flex-1">
										<div class="flex flex-wrap items-center gap-2">
											<strong>{group.items[0].profile_name}</strong>
											<span class="badge badge-outline badge-sm"
												>{group.items.length} failed attempts · same album</span
											>
											{#if group.allStale}
												<span class="badge badge-neutral badge-sm">Superseded</span>
											{:else}
												<span class="badge badge-error badge-sm">Needs attention</span>
											{/if}
										</div>
										{#if group.allStale}
											<p class="mt-1 text-xs text-base-content/55">{STALE_INPUT_HINT}</p>
										{:else if group.staleCount > 0}
											<p class="mt-1 text-xs text-base-content/55">
												{group.staleCount} of {group.items.length} attempts only found moved inputs and
												can be retried; the rest failed for other reasons.
											</p>
										{/if}
										<ul class="mt-2 space-y-1">
											{#each group.items as member (member.operation.id)}
												<li
													class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs {isStaleInputTerminal(
														member.operation
													)
														? 'text-base-content/55'
														: 'text-error'}"
												>
													<span
														>{title(member.mode)} · {isStaleInputTerminal(member.operation)
															? 'Superseded · inputs moved'
															: `${title(member.operation.state)}${member.operation.terminal_code ? ` · ${title(member.operation.terminal_code)}` : ''}`}
														· {date(member.operation.updated_at)}</span
													>
													<a
														class="link link-hover font-semibold"
														href={operationHref(
															member.operation.id,
															member.operation.state,
															member.operation.terminal_code,
															member.mode
														)}>Open details</a
													>
												</li>
											{/each}
										</ul>
										{#if authStore.isAdmin && group.staleCount > 0}
											<div class="mt-2 flex flex-wrap gap-2">
												<Button
													variant="outline"
													size="sm"
													disabled={bulkPending}
													onclick={() => void bulkRetryStale(group.items)}
													>Retry {group.staleCount} stale</Button
												>
												<Button
													variant="danger-ghost"
													size="sm"
													disabled={bulkPending}
													onclick={() => void bulkDismissStale(group.items)}
													>Dismiss {group.staleCount} stale</Button
												>
											</div>
										{/if}
									</div>
								</article>
							{:else}
								{@const single = group.items[0]}
								{@const singleStale = isStaleInputTerminal(single.operation)}
								<a
									href={operationHref(
										single.operation.id,
										single.operation.state,
										single.operation.terminal_code,
										single.mode
									)}
									class="management-history-row"
									><History class="h-4 w-4 text-base-content/45" /><span class="min-w-0 flex-1"
										><strong>{single.profile_name}</strong><small
											>{title(single.mode)} · {singleStale
												? 'Superseded · inputs moved'
												: title(single.operation.state)} · {date(
												single.operation.updated_at
											)}</small
										>
										{#if singleStale}
											<span class="mt-1 block text-xs text-base-content/55">{STALE_INPUT_HINT}</span
											>
										{/if}
										<span class="mt-1 flex flex-wrap gap-1"
											>{#if single.operation.succeeded_count}<span
													class="badge badge-success badge-sm"
													>{single.operation.succeeded_count} succeeded</span
												>{/if}{#if single.operation.failed_count}<span
													class="badge {singleStale ? 'badge-ghost' : 'badge-error'} badge-sm"
													>{single.operation.failed_count} failed</span
												>{/if}{#if single.operation.skipped_count}<span
													class="badge badge-warning badge-sm"
													>{single.operation.skipped_count} skipped</span
												>{/if}</span
										></span
									><ArrowRight class="h-4 w-4" /></a
								>
							{/if}
						{/each}
						{#if bulkSummary}
							<p class="text-sm text-base-content/60" role="status">{bulkSummary}</p>
						{/if}
					{:else}
						<div
							class="rounded-xl border border-dashed border-base-content/15 p-4 text-sm text-base-content/45"
						>
							No organization work has run yet.
						</div>
					{/if}
				</div>
			</section>

			{#if recoveryQuery.data && (recoveryQuery.data.needs_attention_count || recoveryQuery.data.cleanup_pending_count)}<div
					class="alert alert-warning items-start"
				>
					<TriangleAlert class="mt-0.5 h-5 w-5" /><span
						><strong>Some file writes need you</strong><br />{recoveryQuery.data
							.needs_attention_count} files in {needsAttentionBundles.length} writes could not be finished
						or undone safely; {recoveryQuery.data.cleanup_pending_count} have cleanup still to retry.
						Nothing uncertain is ever deleted. Check that each file below is where it should be, then
						mark the write as handled.</span
					>
					{#if authStore.isAdmin && needsAttentionBundles.length > 0}<ul
							class="flex w-full flex-col gap-2"
						>
							{#each needsAttentionBundles as bundle (bundle.bundle_id)}<li
									class="flex flex-wrap items-center justify-between gap-2 rounded-md bg-base-100/60 p-2"
								>
									<span class="min-w-0 text-sm"
										>{bundle.file_count ?? 0} files{#if bundle.sample_paths?.length}: <span
												class="font-mono text-xs break-all">{bundle.sample_paths.join(', ')}</span
											>{/if}</span
									><Button
										variant="warning"
										size="sm"
										aria-label={`Mark ${bundle.bundle_id} as handled`}
										disabled={resolvingBundleId !== null}
										onclick={() => void resolveNeedsAttentionBundle(bundle.bundle_id)}
										>Mark as handled</Button
									>
								</li>{/each}
						</ul>{/if}
				</div>{:else if recoveryUnavailable}<div class="alert alert-error items-start" role="alert">
					<TriangleAlert class="mt-0.5 h-5 w-5" /><span
						><strong>Recovery status is unavailable</strong><br />Do not start new file writes until
						diagnostics load successfully. Refresh this page or check the server logs.</span
					>
				</div>{/if}
			{#if retryingBundles.length > 0}<div class="alert alert-warning items-start">
					<TriangleAlert class="mt-0.5 h-5 w-5" /><span
						><strong>Some file writes are still being retried</strong><br />{retryingBundles.length}
						{retryingBundles.length === 1 ? 'write' : 'writes'} started more than a day ago
						{retryingBundles.length === 1 ? 'is' : 'are'} still not finished and could not be undone either.
						DroppedNeedle tries again every hour and at each start, and has deleted nothing. The usual
						cause is a library folder that is missing, read-only or full: check the folders these files
						are in, and the server log.</span
					>
					<ul class="flex w-full flex-col gap-2">
						{#each retryingBundles as bundle (bundle.bundle_id)}<li
								class="rounded-md bg-base-100/60 p-2 text-sm"
							>
								{bundle.file_count ?? 0} files{#if bundle.sample_paths?.length}: <span
										class="font-mono text-xs break-all">{bundle.sample_paths.join(', ')}</span
									>{/if}
							</li>{/each}
					</ul>
				</div>{/if}
			{#if leftoverFileCount > 0}<div class="alert alert-warning items-start">
					<TriangleAlert class="mt-0.5 h-5 w-5" /><span
						><strong>Leftover temporary files found</strong><br />{leftoverFileCount} hidden
						{leftoverFileCount === 1 ? 'file' : 'files'} in your library folders
						{leftoverFileCount === 1 ? 'was' : 'were'} left by a file write that was interrupted, and
						no record says which write. DroppedNeedle has not deleted or moved any of them and will not.
						Each name starts with <code>.droppedneedle-management-</code> and an id, then the file's
						own name (cut short if it is long), and an ending that says what it is:
						<code>.bak</code>
						is a file as it was before the write and is complete; <code>.tmp</code>, or
						<code>.staging</code> before the file type, is a copy that was still being made and may be
						cut short. Look in the same folder for the file with the normal name. If it is there and plays,
						the hidden file is spare: delete it. If it is missing, copy the hidden file to the normal
						name and check the copy plays before you delete the hidden one. If the normal file is there
						but broken, keep both until one of them plays. This list is from the last server start and
						is checked again at the next one.</span
					>
					<ul class="flex w-full flex-col gap-1">
						{#each leftoverFiles as file (`${file.root_id}/${file.rel_path}`)}<li
								class="rounded-md bg-base-100/60 p-2 font-mono text-xs break-all"
							>
								{file.root_path.replace(/\/+$/, '')}/{file.rel_path}
							</li>{/each}
					</ul>
					{#if leftoverFileCount > leftoverFiles.length}<span class="text-sm"
							>Showing the first {leftoverFiles.length}.</span
						>{/if}
				</div>{/if}
			{#if authStore.isAdmin}
				<QueryBoundary
					query={takeoversQuery}
					what="albums left unmanaged"
					isEmpty={(data) => data.total === 0 && data.handing_back === 0}
				>
					{#snippet loading()}{/snippet}
					{#snippet empty()}{/snippet}
					{#snippet children(takeovers)}
						<details class="rounded-lg border border-base-content/10 p-3">
							<summary class="cursor-pointer text-sm font-semibold"
								>{#if takeovers.total > 0}{takeovers.total}
									{takeovers.total === 1
										? 'album is left unmanaged because its files were'
										: 'albums are left unmanaged because their files were'} changed outside DroppedNeedle{:else}{takeovers.handing_back}
									{takeovers.handing_back === 1 ? 'album is' : 'albums are'} being handed back.{/if}</summary
							>
							{#if takeovers.total > 0 && takeovers.handing_back > 0}
								<p class="mt-2 text-sm text-base-content/70" role="status">
									{takeovers.handing_back}
									{takeovers.handing_back === 1 ? 'album is' : 'albums are'} being handed back.
								</p>
							{/if}
							{#if takeovers.total > 1}
								<div class="mt-3 flex flex-wrap items-center gap-3">
									<AsyncButton
										variant="outline"
										size="sm"
										pending={handBackAll.isPending}
										onclick={() => handBackAll.mutateAsync().catch(() => undefined)}
										>Manage all {takeovers.total} again</AsyncButton
									>
									<p class="max-w-2xl flex-1 text-sm text-base-content/70">
										DroppedNeedle will retag and rename the files of all {takeovers.total} albums from
										their editions. Changes made outside DroppedNeedle are overwritten. Each run can be
										undone from the operations page.
									</p>
								</div>
							{/if}
							{#if takeovers.items.length > 0}
								<ul class="mt-3 flex flex-col gap-2 text-sm">
									{#each takeovers.items as album (album.album_id)}<li
											class="flex flex-wrap items-center justify-between gap-2"
										>
											<span class="min-w-0"
												><a
													class="link font-medium"
													href={withBasePath(`/album/${encodeURIComponent(album.album_id)}`)}
													>{album.album_title}</a
												><span class="text-base-content/60"
													>{album.artist_name ? ` · ${album.artist_name}` : ''} · {day(
														album.noticed_at
													)}</span
												>{#if handBackRefusals[album.album_id]}<span
														class="block text-warning"
														role="status">{handBackRefusals[album.album_id]}</span
													>{/if}</span
											><AsyncButton
												variant="ghost"
												size="sm"
												onclick={() => handBackOne(album.album_id)}>Manage it again</AsyncButton
											>
										</li>{/each}
								</ul>
								<p class="mt-3 text-sm text-base-content/60">
									DroppedNeedle will retag and rename an album's files from its edition. Changes
									made outside DroppedNeedle are overwritten. The run can be undone from the
									operations page.
								</p>
							{/if}
						</details>
					{/snippet}
				</QueryBoundary>
				<QueryBoundary
					query={retagPairsQuery}
					what="albums that might be one album"
					isEmpty={(data) => data.total === 0}
				>
					{#snippet loading()}{/snippet}
					{#snippet empty()}{/snippet}
					{#snippet children(retagPairs)}
						<details class="rounded-lg border border-base-content/10 p-3">
							<summary class="cursor-pointer text-sm font-semibold"
								>{retagPairs.total}
								{retagPairs.total === 1
									? 'pair of albums might be one album'
									: 'pairs of albums might each be one album'} retagged</summary
							>
							<div class="mt-3 flex flex-col gap-3">
								{#each retagPairs.pairs as pair (`${pair.old_album.id}/${pair.new_album.id}`)}
									<RetagPairCard {pair} />
								{/each}
							</div>
							{#if retagPairs.total > retagPairs.pairs.length}
								<p class="mt-3 text-sm text-base-content/60">
									Showing the newest {retagPairs.pairs.length}.
								</p>
							{/if}
						</details>
					{/snippet}
				</QueryBoundary>
			{/if}
			{#if skippedAlbums.length > 0}<details class="rounded-lg border border-base-content/10 p-3">
					<summary class="cursor-pointer text-sm font-semibold"
						>Automatic management skipped {skippedAlbums.length}
						{skippedAlbums.length === 1 ? 'album' : 'albums'}</summary
					>
					<ul class="mt-2 flex flex-col gap-2 text-sm">
						{#each skippedAlbums as album (album.album_id)}<li>
								<a class="link font-medium" href={`/album/${encodeURIComponent(album.album_id)}`}
									>{album.album_title}</a
								>{#if album.artist_name}<span class="text-base-content/60">
										· {album.artist_name}</span
									>{/if}
								<p class="text-base-content/80">{album.reason.message}</p>
								<p class="text-base-content/60">{album.reason.action}</p>
							</li>{/each}
					</ul>
				</details>{/if}
		</div>
	{/if}
</section>

{#if runnerMode && settingsQuery.data && policyQuery.data}
	<LibraryManagementRunner
		mode={runnerMode}
		roots={policyQuery.data.library_roots}
		settings={settingsQuery.data}
		policyRevision={policyQuery.data.policy_revision}
		onclose={closeRunner}
	/>
{/if}

<style>
	.management-control-stat {
		display: grid;
		gap: 0.2rem;
		min-height: 7rem;
		padding: 1rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
		border-radius: 1rem;
		background: color-mix(in oklab, var(--color-base-200) 40%, transparent);
	}

	.management-control-stat[data-attention='true'] {
		border-color: color-mix(in oklab, var(--color-warning) 40%, transparent);
	}

	.management-control-stat span {
		font-size: 0.7rem;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: color-mix(in oklab, var(--color-base-content) 48%, transparent);
	}

	.management-control-stat strong {
		font-family: var(--font-display);
		font-size: 1.15rem;
	}

	.management-control-stat small {
		align-self: end;
		color: color-mix(in oklab, var(--color-base-content) 48%, transparent);
	}

	.management-active-card {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 20%, transparent);
		border-radius: 1rem;
		background: color-mix(in oklab, var(--color-library-manage) 6%, var(--color-base-100));
	}

	.management-active-card {
		flex-wrap: wrap;
		justify-content: space-between;
		padding: 1rem;
	}

	@media (max-width: 639px) {
		.management-active-card {
			align-items: stretch;
			flex-direction: column;
		}
	}
</style>
