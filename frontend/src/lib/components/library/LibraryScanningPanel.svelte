<script lang="ts">
	import {
		CirclePause,
		CirclePlay,
		FolderSync,
		Fingerprint,
		ListChecks,
		OctagonX,
		RefreshCw,
		ScanLine,
		Settings2,
		ShieldAlert
	} from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryActivityQuery } from '$lib/queries/library/LibraryActivityQueries.svelte';
	import {
		getCurrentLibraryRunsQuery,
		getLibraryRunQuery
	} from '$lib/queries/library/LibraryOperationQueries.svelte';
	import {
		controlIdentification,
		controlLibraryRun,
		requestLibraryRun
	} from '$lib/queries/library/LibraryOperationMutations.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { getLibraryReviewsQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import { getArtistReconciliationProgressQuery } from '$lib/queries/artist-reconciliation/ArtistReconciliationQueries.svelte';
	import { getLibraryScanScheduleQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import LibraryWorkLane from './LibraryWorkLane.svelte';
	import LibraryWorkDialog from './LibraryWorkDialog.svelte';
	import LibraryRunHistory from './LibraryRunHistory.svelte';

	const activityQuery = getLibraryActivityQuery(() => authStore.user?.id);
	const runsQuery = getCurrentLibraryRunsQuery(() => authStore.isAdmin);
	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const scheduleQuery = getLibraryScanScheduleQuery(() => authStore.isAdmin);
	const reviewsQuery = getLibraryReviewsQuery(() => ({ state: 'needs_review' }));
	const artistReconciliationQuery = getArtistReconciliationProgressQuery(() => authStore.isAdmin);
	const runDetailQuery = getLibraryRunQuery(() => runsQuery.data?.active?.id ?? null);
	const requestRun = requestLibraryRun();
	const pauseRun = controlLibraryRun('pause');
	const resumeRun = controlLibraryRun('resume');
	const stopRun = controlLibraryRun('stop');
	const pauseIdentification = controlIdentification('pause');
	const resumeIdentification = controlIdentification('resume');

	const scan = $derived(activityQuery.data?.items.find((item) => item.kind === 'scan'));
	const identification = $derived(
		activityQuery.data?.items.find((item) => item.kind === 'identification')
	);
	const activeRun = $derived(runsQuery.data?.active ?? null);
	const queuedRun = $derived(runsQuery.data?.queued ?? null);
	const counters = $derived(runDetailQuery.data?.snapshot.counters ?? {});
	const scopes = $derived(runDetailQuery.data?.snapshot.scopes ?? []);
	const policyRevision = $derived(settingsQuery.data?.policy_revision ?? '');
	const libraryEnabled = $derived(settingsQuery.data?.enabled ?? true);
	const reviewCount = $derived(reviewsQuery.data?.pages[0]?.filtered_total ?? 0);
	const roots = $derived(settingsQuery.data?.library_roots ?? []);
	const scopeLabel = $derived(
		activeRun?.aggregate_scope === 'all'
			? 'Whole library'
			: [
					...new Set(
						scopes.map(
							(scope) => roots.find((root) => root.id === scope.root_id)?.label ?? scope.root_id
						)
					)
				].join(', ') || 'Selected roots'
	);
	const elapsed = $derived(
		activeRun?.started_at ? Math.max(0, Date.now() / 1000 - activeRun.started_at) : 0
	);
	const throughput = $derived(elapsed > 5 && scan ? scan.processed / elapsed : null);
	const eta = $derived(
		throughput && scan?.total && scan.total > scan.processed
			? (scan.total - scan.processed) / throughput
			: null
	);
	let stopDialog: HTMLDialogElement;
	let stopHeading: HTMLHeadingElement;
	let stopOpener: HTMLButtonElement | null = null;
	let workDialogOpen = $state(false);
	let workKind = $state<'rescan_files' | 'policy_reconcile' | 'retry_identification'>(
		'rescan_files'
	);
	const catalogRevision = $derived(reviewsQuery.data?.pages[0]?.catalog_revision ?? 0);
	const attentionCount = $derived(identification?.attention_count ?? 0);
	const deferredReasonBreakdown = $derived(
		Object.entries(identification?.deferred_reason_counts ?? {})
			.map(([code, count]) => `${deferredReasonLabel(code)}: ${count.toLocaleString()}`)
			.join(', ')
	);
	const deferredJobs = $derived((identification?.deferred_jobs ?? []).slice(0, 5));
	const hiddenDeferredJobs = $derived(
		Math.max(0, (identification?.deferred_jobs?.length ?? 0) - 5)
	);

	function deferredReasonLabel(code: string): string {
		const labels: Record<string, string> = {
			PROVIDER_TEMPORARILY_UNAVAILABLE: 'provider temporarily unavailable',
			UNMAPPABLE_PROVIDER_PAYLOAD:
				'provider response could not be mapped (data problem, not an outage)',
			SUBJECT_NOT_AVAILABLE: 'album no longer available',
			MAX_DEFERRALS_EXCEEDED: 'retry limit reached'
		};
		return labels[code] ?? code.toLowerCase().replaceAll('_', ' ');
	}

	function stateLabel(state: string): string {
		const labels: Record<string, string> = {
			idle: 'Idle',
			discovering: 'Counting local files',
			indexing: 'Indexing local files',
			reconciling: 'Reconciling library',
			pausing: 'Pausing after the current file...',
			paused: 'Paused',
			stopping: 'Stopping after the current file...',
			superseded_policy_changed: 'Stopped because library policy changed',
			failed: 'Failed'
		};
		return labels[state] ?? state.replaceAll('_', ' ');
	}

	function formatDuration(seconds: number): string {
		if (seconds < 60) return `${Math.round(seconds)}s`;
		if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
		return `${(seconds / 3600).toFixed(1)}h`;
	}

	function formatAge(startedAt: number | null | undefined): string {
		if (!startedAt) return '-';
		return formatDuration(Math.max(0, Date.now() / 1000 - startedAt));
	}

	async function startRun(
		kind: 'incremental' | 'rescan_files' | 'policy_reconcile',
		scopeIds: string[] = []
	): Promise<void> {
		try {
			await requestRun.mutateAsync({
				kind,
				scope_ids: scopeIds,
				expected_policy_revision: policyRevision
			});
		} catch {
			return;
		}
		workDialogOpen = false;
	}

	function openWork(kind: 'rescan_files' | 'policy_reconcile' | 'retry_identification'): void {
		workKind = kind;
		workDialogOpen = true;
	}
</script>

<section id="scanning-controls" class="space-y-5" aria-label="Scanning and identification">
	<div
		class="rounded-3xl border border-primary/15 bg-gradient-to-br from-primary/10 via-base-200/50 to-base-200/40 p-5 sm:p-6"
	>
		<div class="flex flex-wrap items-center justify-between gap-3">
			<p class="library-scanning-kicker">Read-only catalog work</p>
			<div class="flex flex-wrap items-center gap-2">
				<p class="library-scanning-schedule">
					{#if scheduleQuery.data?.scan_frequency === 'daily'}Next scan: {scheduleQuery.data
							.daily_scan_time}
						{scheduleQuery.data.server_timezone ??
							''}{:else if scheduleQuery.data?.scan_frequency === 'manual'}Automatic scanning off{:else}Schedule:
						{scheduleQuery.data?.scan_frequency?.replace('_', ' ') ?? 'loading'}{/if}
				</p>
				<a href={withBasePath('/settings?tab=library')} class="btn btn-ghost btn-sm">
					<Settings2 class="h-4 w-4" /> Settings
				</a>
			</div>
		</div>
		{#if !libraryEnabled}
			<div class="alert alert-warning">
				<ShieldAlert class="h-5 w-5" />
				<div class="min-w-0 flex-1">
					<strong>The local library is disabled</strong>
					<p class="text-sm">
						Scanning and identification are paused. Existing catalog data and playback keep working.
						Enable the library in
						<a class="link link-primary" href={withBasePath('/settings?tab=library')}>Settings</a> to
						start new work.
					</p>
				</div>
			</div>
		{:else}
			<div class="mt-4 flex flex-wrap items-center justify-between gap-4">
				<div class="max-w-xl">
					<h2 class="font-display text-2xl font-bold">Scan &amp; identify</h2>
					<p class="mt-1 text-sm text-base-content/60">
						Reads your library folders and updates the catalog. Identification then matches albums
						to MusicBrainz. Nothing here writes to your files.
					</p>
				</div>
				<button
					class="btn btn-primary btn-lg rounded-full shadow-lg shadow-primary/25 glow-primary-soft"
					disabled={requestRun.isPending || !policyRevision}
					onclick={() => void startRun('incremental')}
				>
					<RefreshCw class="h-5 w-5" /> Scan for changes
				</button>
			</div>
			<div class="mt-4 space-y-3">
				<div
					class="flex flex-wrap items-center justify-between gap-3 border-t border-base-content/10 pt-3 text-sm"
				>
					<button class="btn btn-outline btn-sm" onclick={() => openWork('rescan_files')}>
						<ScanLine class="h-4 w-4" /> Rescan files...
					</button>
					<p class="text-base-content/55">
						Deep re-read of selected folders. Use after fixing files outside DroppedNeedle.
					</p>
				</div>
				<div
					class="flex flex-wrap items-center justify-between gap-3 border-t border-base-content/10 pt-3 text-sm"
				>
					<button class="btn btn-outline btn-sm" onclick={() => openWork('retry_identification')}>
						<ListChecks class="h-4 w-4" /> Retry identification...
					</button>
					<p class="text-base-content/55">
						Re-runs matching for albums still waiting on review or a provider.
					</p>
				</div>
			</div>
		{/if}
	</div>

	{#if activityQuery.isLoading || runsQuery.isLoading}
		<div class="space-y-3">
			<div class="skeleton h-40 rounded-box"></div>
			<div class="skeleton h-40 rounded-box"></div>
		</div>
	{:else if activityQuery.isError || runsQuery.isError}
		<div class="alert alert-error">Could not load library operations.</div>
	{:else}
		<div class="space-y-3">
			<article class="overflow-hidden rounded-box border border-base-content/15 bg-base-200/40">
				<div class="flex flex-wrap items-center gap-3 border-b border-base-content/10 px-4 py-3">
					<div
						class="flex h-9 w-9 items-center justify-center rounded-xl bg-[color-mix(in_oklab,var(--color-library-index)_16%,transparent)] text-[var(--color-library-index)]"
					>
						<FolderSync class="h-4 w-4" />
					</div>
					<div class="min-w-0 flex-1">
						<h3 class="font-semibold">Local files</h3>
						<p class="text-sm text-base-content/55">
							{scan ? stateLabel(scan.state) : 'Idle'}{#if activeRun}
								· {activeRun.trigger.replaceAll('_', ' ')} · {scopeLabel}{/if}
						</p>
					</div>
					{#if activeRun?.state === 'paused'}
						<button
							class="btn btn-outline btn-sm"
							disabled={resumeRun.isPending}
							onclick={() =>
								void resumeRun
									.mutateAsync({
										runId: activeRun.id,
										expectedRevision: activeRun.row_revision
									})
									.catch(() => undefined)}
							aria-label="Resume local scan"><CirclePlay class="h-4 w-4" /> Resume</button
						>
					{:else if activeRun && !['pausing', 'stopping'].includes(activeRun.state)}
						<button
							class="btn btn-outline btn-sm"
							disabled={pauseRun.isPending}
							onclick={() =>
								void pauseRun
									.mutateAsync({
										runId: activeRun.id,
										expectedRevision: activeRun.row_revision
									})
									.catch(() => undefined)}
							aria-label="Pause local scan"><CirclePause class="h-4 w-4" /> Pause</button
						>
					{/if}
					{#if activeRun}<button
							class="btn btn-ghost btn-sm text-error"
							onclick={(event) => {
								stopOpener = event.currentTarget;
								stopDialog.showModal();
								stopHeading.focus();
							}}
							aria-label="Stop local scan"><OctagonX class="h-4 w-4" /> Stop</button
						>{/if}
				</div>
				<div class="space-y-3 p-4">
					<LibraryWorkLane kind="scan" item={scan} />
					<div class="grid grid-cols-2 gap-2 text-sm sm:grid-cols-4 lg:grid-cols-9">
						<div>
							<span class="block text-xs text-base-content/50">New</span><strong
								>{(counters.new_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Elapsed</span><strong
								>{activeRun?.started_at ? formatDuration(elapsed) : '-'}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Changed</span><strong
								>{(counters.changed_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Unchanged</span><strong
								>{(counters.unchanged_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Excluded</span><strong
								>{(counters.excluded_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Missing</span><strong
								>{(counters.missing_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Errors</span><strong
								>{(counters.errored_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Rate</span><strong
								>{throughput ? `${throughput.toFixed(1)}/s` : '-'}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">ETA</span><strong
								>{eta && elapsed > 30 ? formatDuration(eta) : '-'}</strong
							>
						</div>
					</div>
					{#if scopes.length}<details class="text-sm">
							<summary class="cursor-pointer font-medium">Root progress and phase details</summary>
							{#if activeRun}
								<p class="mt-2 text-base-content/60">
									Current phase: <strong>{stateLabel(activeRun.state)}</strong>
								</p>
							{/if}
							<ul class="mt-2 space-y-1 text-base-content/60">
								{#each scopes as scope (scope.scope_id)}<li>
										{roots.find((root) => root.id === scope.root_id)?.label ?? scope.root_id} · {scope.effective_policy.replace(
											'_',
											' '
										)}
										{#if scope.estimated_count !== null}
											· about {scope.estimated_count.toLocaleString()} files{/if}
									</li>{/each}
							</ul>
							{#if activeRun && Object.keys(activeRun.phase_timings).length}
								<p class="mt-2 text-xs font-semibold text-base-content/50">
									Completed phase timings
								</p>
								<ul class="mt-2 space-y-1 text-base-content/60">
									{#each Object.entries(activeRun.phase_timings) as [phase, seconds] (phase)}
										<li>{phase.replaceAll('_', ' ')} · {formatDuration(seconds)}</li>
									{/each}
								</ul>
							{/if}
						</details>{/if}
					{#if queuedRun}
						<p class="rounded-box bg-info/10 p-3 text-sm text-info-content">
							Queued follow-up: {queuedRun.kind.replaceAll('_', ' ')} will start after the active scan.
						</p>
					{/if}
					{#if activeRun?.terminal_code === 'POLICY_CHANGED' || activeRun?.state === 'superseded_policy_changed'}<div
							class="alert alert-warning text-sm"
						>
							<ShieldAlert class="h-4 w-4" /><span>Stopped because library policy changed</span
							><button class="btn btn-sm" onclick={() => openWork('policy_reconcile')}
								>Apply policy changes...</button
							>
						</div>{/if}
				</div>
			</article>

			<article class="overflow-hidden rounded-box border border-base-content/15 bg-base-200/40">
				<div class="flex flex-wrap items-center gap-3 border-b border-base-content/10 px-4 py-3">
					<div
						class="flex h-9 w-9 items-center justify-center rounded-xl bg-[color-mix(in_oklab,var(--color-library-identify)_16%,transparent)] text-[var(--color-library-identify)]"
					>
						<ScanLine class="h-4 w-4" />
					</div>
					<div class="min-w-0 flex-1">
						<h3 class="font-semibold">Identification</h3>
						<p class="text-sm text-base-content/55">
							{identification ? stateLabel(identification.state) : 'Idle'}
						</p>
					</div>
					{#if identification?.state === 'paused' && identification.control_revision}<button
							class="btn btn-outline btn-sm"
							disabled={resumeIdentification.isPending}
							onclick={() =>
								void resumeIdentification
									.mutateAsync(identification.control_revision ?? 0)
									.catch(() => undefined)}
							aria-label="Resume identification"><CirclePlay class="h-4 w-4" /> Resume</button
						>{:else if identification?.waiting_count && identification.control_revision}<button
							class="btn btn-outline btn-sm"
							disabled={pauseIdentification.isPending || identification.state === 'pausing'}
							onclick={() =>
								void pauseIdentification
									.mutateAsync(identification.control_revision ?? 0)
									.catch(() => undefined)}
							aria-label="Pause identification"
							><CirclePause class="h-4 w-4" />
							{identification.state === 'pausing' ? 'Pausing...' : 'Pause'}</button
						>{/if}
				</div>
				<div class="space-y-3 p-4">
					<LibraryWorkLane kind="identification" item={identification} />
					<div class="grid grid-cols-2 gap-2 text-sm sm:grid-cols-4 lg:grid-cols-7">
						<div>
							<span class="block text-xs text-base-content/50">Complete</span><strong
								>{(identification?.processed ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Identified</span><strong
								>{(identification?.identified_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Waiting</span><strong
								>{(identification?.waiting_count ?? 0).toLocaleString()}</strong
							>
							<span class="block text-xs text-base-content/50">will auto-retry</span>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Kept local</span><strong
								>{(identification?.kept_local_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Failed</span><strong
								>{(identification?.failed_count ?? 0).toLocaleString()}</strong
							>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Needs decision</span><strong
								>{(identification?.needs_review_count ?? reviewCount).toLocaleString()}</strong
							>
							<span class="block text-xs text-base-content/50">will not auto-resolve</span>
						</div>
						<div>
							<span class="block text-xs text-base-content/50">Deferred</span><strong
								>{(identification?.deferred_count ?? 0).toLocaleString()}</strong
							>
						</div>
					</div>
					<div class="flex flex-wrap gap-x-5 gap-y-1 text-xs text-base-content/60">
						<span>Current work: {identification?.priority_band ?? 'No queued priority'}</span>
						<span>Oldest waiting: {formatAge(identification?.oldest_backlog_at)}</span>
					</div>
					{#if attentionCount > 0}
						<div class="alert alert-error py-2 text-sm">
							{attentionCount.toLocaleString()}
							{attentionCount === 1 ? 'album needs' : 'albums need'} attention after repeated identification
							failures or because the album is no longer available. Use Review identification below to
							resolve them.
						</div>
					{:else if identification?.provider_unavailable}
						<div class="alert alert-warning py-2 text-sm">
							MusicBrainz is currently unavailable. Metadata checks will resume automatically when
							it recovers. Local playback is unaffected.
						</div>
					{:else if (identification?.deferred_count ?? 0) > 0}
						<div class="alert alert-info py-2 text-sm">
							{(identification?.deferred_count ?? 0).toLocaleString()} metadata
							{(identification?.deferred_count ?? 0) === 1 ? 'check is' : 'checks are'} deferred{deferredReasonBreakdown
								? ` (${deferredReasonBreakdown})`
								: ''}. They retry automatically.
							{#if deferredJobs.length > 0}
								<ul class="mt-1 space-y-0.5">
									{#each deferredJobs as job (job.job_id)}
										<li class="text-xs">
											<span class="font-medium">{job.album_title ?? 'Track-level work'}</span
											>{#if job.artist_name}<span class="text-base-content/55">
													- {job.artist_name}</span
												>{/if}
											<span class="text-base-content/50">
												· {deferredReasonLabel(job.last_failure_code)} · attempt {job.attempt_count.toLocaleString()}</span
											>
										</li>
									{/each}
									{#if hiddenDeferredJobs > 0}
										<li class="text-xs text-base-content/50">
											and {hiddenDeferredJobs.toLocaleString()} more
										</li>
									{/if}
								</ul>
							{/if}
						</div>
					{/if}
					<div class="flex flex-wrap items-center justify-between gap-2 text-sm">
						<span class="text-base-content/55"
							>Provider work runs in the background without delaying local playback.</span
						><a class="link link-primary" href={withBasePath('/library/review?state=needs_review')}
							>Review identification</a
						>
					</div>
				</div>
			</article>

			<a
				href={withBasePath('/library/management/artists')}
				class="flex flex-wrap items-center gap-4 rounded-box border border-base-content/15 bg-base-200/40 p-4 hover:bg-base-200"
			>
				<div
					class="flex h-10 w-10 items-center justify-center rounded-xl bg-primary/10 text-primary"
				>
					<Fingerprint class="h-5 w-5" />
				</div>
				<div class="min-w-0 flex-1">
					<h3 class="font-semibold">Artist identity desk</h3>
					<p class="text-sm text-base-content/55">
						Provider-proven reconciliation and duplicate groups that need administrator judgement.
					</p>
				</div>
				<div class="flex gap-5 text-sm">
					<span
						><strong
							>{(
								artistReconciliationQuery.data?.automatically_resolved_count ?? 0
							).toLocaleString()}</strong
						><span class="block text-xs text-base-content/50">resolved</span></span
					>
					<span
						><strong
							>{(
								artistReconciliationQuery.data?.waiting_for_identity_count ?? 0
							).toLocaleString()}</strong
						><span class="block text-xs text-base-content/50">waiting</span></span
					>
					<span
						><strong
							>{(
								artistReconciliationQuery.data?.genuine_review_count ?? 0
							).toLocaleString()}</strong
						><span class="block text-xs text-base-content/50">review</span></span
					>
				</div>
			</a>
		</div>
	{/if}

	<div id="recent-runs"><LibraryRunHistory /></div>
</section>

<LibraryWorkDialog
	open={workDialogOpen}
	kind={workKind}
	{catalogRevision}
	pending={requestRun.isPending}
	onclose={() => (workDialogOpen = false)}
	onconfirm={(scopeIds) =>
		workKind === 'retry_identification' ? Promise.resolve() : startRun(workKind, scopeIds)}
/>

<dialog
	bind:this={stopDialog}
	class="modal"
	aria-labelledby="stop-scan-title"
	onclose={() => stopOpener?.focus()}
>
	<div class="modal-box max-w-md">
		<h2 bind:this={stopHeading} id="stop-scan-title" tabindex="-1" class="text-lg font-bold">
			Stop this scan?
		</h2>
		<p class="mt-3 text-sm text-base-content/70">
			Files already indexed will stay available. Unfinished work is discarded; the next scan
			compares the library again.
		</p>
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => stopDialog.close()}>Keep scanning</button><button
				class="btn btn-error"
				disabled={stopRun.isPending || !activeRun}
				onclick={async () => {
					try {
						if (activeRun)
							await stopRun.mutateAsync({
								runId: activeRun.id,
								expectedRevision: activeRun.row_revision
							});
					} catch {
						return;
					}
					stopDialog.close();
				}}
				>{#if stopRun.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Stop scan</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close stop dialog">close</button>
	</form>
</dialog>
