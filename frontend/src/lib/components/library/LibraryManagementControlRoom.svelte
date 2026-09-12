<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import { onMount } from 'svelte';
	import { SvelteURL } from 'svelte/reactivity';
	import {
		TriangleAlert,
		ArrowRight,
		CirclePause,
		CirclePlay,
		History,
		RotateCcw,
		Settings2,
		Sparkles
	} from 'lucide-svelte';

	import LibraryManagementRunner from './LibraryManagementRunner.svelte';
	import LibraryManagementDiscardPreview from './LibraryManagementDiscardPreview.svelte';
	import LibraryManagementIdentityReadiness from './LibraryManagementIdentityReadiness.svelte';
	import LibraryRepairPanel from './LibraryRepairPanel.svelte';
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
		reissueLibraryManagementPreviewMutation,
		resolveLibraryManagementImportBundleMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import {
		forgetLibraryManagementPreviewToken,
		rememberLibraryManagementPreviewToken
	} from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import {
		getLibraryManagementOperationsQuery,
		getLibraryManagementRecoveryQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';

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
	let runnerOpener = $state<HTMLButtonElement | null>(null);

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
		groupFailedOperationsByAlbum(recentFailed, (item) =>
			isStaleInputTerminal(item.operation)
		)
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
	const attentionCount = $derived(
		(recoveryQuery.data?.needs_attention_count ?? 0) +
			(recoveryQuery.data?.cleanup_pending_count ?? 0) +
			history.filter(
				(item) =>
					item.operation.state === 'failed' && !isStaleInputTerminal(item.operation)
			).length
	);
	const recoveryUnavailable = $derived(recoveryQuery.isError);
	const needsAttentionBundles = $derived(recoveryQuery.data?.needs_attention_bundles ?? []);

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

	function openRunner(mode: RunnerMode, opener: HTMLButtonElement): void {
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
				message: `Import bundle marked as handled (${result.verified_files}/${result.total_files} files verified).`,
				type: 'success'
			});
		} catch (error) {
			toastStore.show({
				message:
					error instanceof Error && error.message
						? error.message
						: 'Could not mark this import bundle as handled',
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
				<button
					class="btn management-btn"
					disabled={recoveryUnavailable || !settingsQuery.data || !policyQuery.data}
					onclick={(event) => openRunner('manage', event.currentTarget)}
					><Sparkles class="h-4 w-4" /> Preview organization...</button
				>
				<button
					class="btn btn-outline"
					disabled={recoveryUnavailable || !settingsQuery.data || !policyQuery.data}
					onclick={(event) => openRunner('baseline_restore', event.currentTarget)}
					><RotateCcw class="h-4 w-4" /> Restore original state...</button
				>
				<a href={withBasePath('/library/management?tab=automation')} class="btn btn-ghost btn-sm"
					><Settings2 class="h-4 w-4" /> Automation</a
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
					<div class="flex flex-wrap gap-1">
						{#if active.operation.state === 'paused'}<button
								class="btn btn-outline btn-sm"
								disabled={resumeOperation.isPending}
								onclick={() =>
									void resumeOperation
										.mutateAsync({
											jobId: active.operation.id,
											expectedRevision: active.operation.row_revision
										})
										.catch(() => undefined)}><CirclePlay class="h-4 w-4" /> Resume</button
							>{:else if active.operation.state === 'running'}<button
								class="btn btn-outline btn-sm"
								disabled={pauseOperation.isPending}
								onclick={() =>
									void pauseOperation
										.mutateAsync({
											jobId: active.operation.id,
											expectedRevision: active.operation.row_revision
										})
										.catch(() => undefined)}><CirclePause class="h-4 w-4" /> Pause</button
							>{/if}<a
							class="btn btn-ghost btn-sm"
							href={operationHref(
								active.operation.id,
								active.operation.state,
								active.operation.terminal_code,
								active.mode
							)}>Open details <ArrowRight class="h-4 w-4" /></a
						>
					</div>
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
												{group.staleCount} of {group.items.length} attempts only found moved
												inputs and can be retried; the rest failed for other reasons.
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
															: `${title(member.operation.state)}${member.operation.terminal_code ? ` · ${title(member.operation.terminal_code)}` : ''}`} · {date(
															member.operation.updated_at
														)}</span
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
												<button
													type="button"
													class="btn btn-outline btn-sm"
													disabled={bulkPending}
													onclick={() => void bulkRetryStale(group.items)}
													>Retry {group.staleCount} stale</button
												>
												<button
													type="button"
													class="btn btn-ghost btn-sm text-error"
													disabled={bulkPending}
													onclick={() => void bulkDismissStale(group.items)}
													>Dismiss {group.staleCount} stale</button
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
												: title(single.operation.state)} · {date(single.operation.updated_at)}</small
										>
										{#if singleStale}
											<span class="mt-1 block text-xs text-base-content/55"
												>{STALE_INPUT_HINT}</span
											>
										{/if}
										<span class="mt-1 flex flex-wrap gap-1"
											>{#if single.operation.succeeded_count}<span
													class="badge badge-success badge-sm"
													>{single.operation.succeeded_count} succeeded</span
												>{/if}{#if single.operation.failed_count}<span
													class="badge {singleStale
														? 'badge-ghost'
														: 'badge-error'} badge-sm"
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
						><strong>Recovery needs attention</strong><br />{recoveryQuery.data
							.needs_attention_count} bundles need review; {recoveryQuery.data
							.cleanup_pending_count} have safe cleanup pending. No uncertain file is deleted automatically.</span
					>
					{#if authStore.isAdmin && needsAttentionBundles.length > 0}<div
							class="flex w-full flex-wrap gap-2"
						>
							{#each needsAttentionBundles as bundle (bundle.bundle_id)}<button
									type="button"
									class="btn btn-warning btn-sm"
									disabled={resolvingBundleId !== null}
									onclick={() => void resolveNeedsAttentionBundle(bundle.bundle_id)}
									>Mark {bundle.bundle_id} as handled</button
								>{/each}
						</div>{/if}
				</div>{:else if recoveryUnavailable}<div class="alert alert-error items-start" role="alert">
					<TriangleAlert class="mt-0.5 h-5 w-5" /><span
						><strong>Recovery status is unavailable</strong><br />Do not start new file writes until
						diagnostics load successfully. Refresh this page or check the server logs.</span
					>
				</div>{/if}
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
