<script lang="ts">
	import {
		TriangleAlert,
		ArrowRight,
		ArrowUpRight,
		CalendarDays,
		ChevronRight,
		CircleCheck,
		CircleX,
		Clock3,
		CircleDashed,
		Folder,
		Layers,
		ListChecks,
		Music2,
		Radio,
		RefreshCw,
		RotateCcw,
		ScanSearch,
		Settings2,
		Sparkles,
		Wrench
	} from 'lucide-svelte';
	import { onMount } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Illustration from '$lib/ui/Illustration.svelte';

	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryActivityQuery } from '$lib/features/library-management/queries/LibraryActivityQueries.svelte';
	import { getLibraryRunHistoryQuery } from '$lib/features/library-management/queries/LibraryOperationQueries.svelte';
	import { createRequestLibraryRunMutation } from '$lib/features/library-management/queries/LibraryOperationMutations.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
	import { getPendingReviewCountQuery } from '$lib/features/library/queries/LibraryReviewQueries.svelte';
	import {
		getLibraryScanScheduleQuery,
		getLibraryStatsQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import {
		getLibraryManagementOperationsQuery,
		getLibraryManagementRecoveryQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/features/library-management/queries/LibraryManagementQueries.svelte';
	import { getLibraryIdentityPreparationEstimateQuery } from '$lib/features/library/queries/LibraryIdentityPreparationQueries.svelte';
	import type { LibraryWorkItem } from '$lib/features/library-management/queries/LibraryOperationsTypes';
	import { withBasePath } from '$lib/utils/basePath';
	import { isRecord, titleManagementValue } from './LibraryManagementDisplay';
	import LibraryWorkIcon from './LibraryWorkIcon.svelte';
	import LibraryWorkProgress from './LibraryWorkProgress.svelte';
	import {
		WAITING_FOR_SCAN_HINT,
		isQueuedPreview,
		libraryWorkContext,
		libraryWorkEffect,
		libraryWorkFacts,
		libraryWorkHref,
		libraryWorkTitle,
		scanIsActive
	} from './LibraryWorkPresentation';

	const activityQuery = getLibraryActivityQuery(() => authStore.user?.id);
	const statsQuery = getLibraryStatsQuery();
	const historyQuery = getLibraryRunHistoryQuery(() => authStore.isAdmin);
	const scheduleQuery = getLibraryScanScheduleQuery(() => authStore.isAdmin);
	const reviewsQuery = getPendingReviewCountQuery();
	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const managementSettingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const operationsQuery = getLibraryManagementOperationsQuery(
		() => authStore.user?.id,
		() => ({ limit: 20 })
	);
	const recoveryQuery = getLibraryManagementRecoveryQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const identityEstimateQuery = getLibraryIdentityPreparationEstimateQuery(
		() => authStore.user?.id,
		() => [],
		() => authStore.isAdmin
	);
	const requestRun = createRequestLibraryRunMutation();

	let now = $state(Date.now() / 1000);

	onMount(() => {
		const timer = window.setInterval(() => (now = Date.now() / 1000), 60_000);
		return () => window.clearInterval(timer);
	});

	const items = $derived(
		(activityQuery.data?.work_items ?? []).filter((item) => item.id !== 'identification-drain')
	);
	const drain = $derived(
		(activityQuery.data?.work_items ?? []).find((item) => item.id === 'identification-drain')
	);
	const identificationActivity = $derived(
		(activityQuery.data?.items ?? []).find((item) => item.kind === 'identification')
	);
	const drainPending = $derived(
		drain?.pending_identification ??
			(identificationActivity?.waiting_count ?? 0) + (identificationActivity?.deferred_count ?? 0)
	);
	const drainActive = $derived(Boolean(drain?.synthetic) && (drainPending ?? 0) > 0);
	const primary = $derived(items[0] ?? null);
	const additional = $derived(items.slice(1));
	const scanActive = $derived(scanIsActive(items));
	const facts = $derived(primary ? libraryWorkFacts(primary) : []);
	const steps = $derived(primary && primary.effect !== 'attention' ? workSteps(primary) : []);
	const effect = $derived(primary?.effect ?? 'idle');

	const policyRevision = $derived(settingsQuery.data?.policy_revision ?? '');
	const libraryEnabled = $derived(settingsQuery.data?.enabled ?? true);
	const roots = $derived(settingsQuery.data?.library_roots ?? []);
	const totalTracks = $derived(statsQuery.data?.total_tracks ?? 0);
	const localOnlyCount = $derived(statsQuery.data?.local_only_count ?? 0);
	const guessCount = $derived(statsQuery.data?.guess_count ?? 0);
	const scanRuns = $derived(historyQuery.data?.pages.flatMap((result) => result.items) ?? []);
	const latestTerminalRun = $derived(scanRuns[0] ?? null);
	const reviewCount = $derived(reviewsQuery.data ?? 0);
	const operations = $derived(operationsQuery.data?.pages.flatMap((result) => result.items) ?? []);
	const readyPreviewCount = $derived(
		operations.filter((item) => item.operation.state === 'ready' && !item.activation_preview).length
	);
	const attentionCount = $derived(
		(recoveryQuery.data?.needs_attention_count ?? 0) +
			(recoveryQuery.data?.cleanup_pending_count ?? 0)
	);
	const identityShortfall = $derived(
		(identityEstimateQuery.data?.mapping_required_count ?? 0) +
			(identityEstimateQuery.data?.exact_release_required_count ?? 0)
	);
	const schedule = $derived(scheduleQuery.data ?? null);
	const scheduleText = $derived(
		schedule?.scan_frequency === 'daily'
			? `Next scan: ${schedule.daily_scan_time} ${schedule.server_timezone ?? ''}`
			: schedule?.scan_frequency === 'manual'
				? 'Scheduled scans off (file watcher still active)'
				: `Schedule: ${schedule?.scan_frequency?.replace('_', ' ') ?? 'loading'}`
	);
	const frequencyLabels: Record<string, string> = {
		'30min': 'every 30 minutes',
		'1hr': 'every hour',
		'6hr': 'every 6 hours',
		'12hr': 'every 12 hours',
		'24hr': 'every 24 hours',
		'3d': 'every 3 days',
		'7d': 'every 7 days',
		daily: 'daily'
	};
	const gapLabels: Record<string, string> = {
		'30min': '30 minutes',
		'1hr': '1 hour',
		'6hr': '6 hours',
		'12hr': '12 hours',
		'24hr': '24 hours',
		'3d': '3 days',
		'7d': '7 days'
	};
	// A rolling schedule counts its gap from the last scan.
	const rollingGap = $derived(schedule ? (gapLabels[schedule.scan_frequency] ?? null) : null);
	const scheduleLead = $derived(
		rollingGap ? `Next scan: ${rollingGap} after the last` : scheduleText
	);
	const scheduleGlance = $derived(scheduleLead.replace(/^(Next scan|Schedule): /, ''));
	const scheduleDetail = $derived(
		schedule && schedule.scan_frequency !== 'manual'
			? `Schedule: ${frequencyLabels[schedule.scan_frequency] ?? schedule.scan_frequency} (automatic)`
			: null
	);

	// Which roots DroppedNeedle may organize by itself, from their assignments.
	const automaticRootIds = $derived(
		new Set(
			(managementSettingsQuery.data?.root_assignments ?? [])
				.filter(
					(assignment) =>
						assignment.enabled &&
						(assignment.automatic_acquisitions ||
							assignment.automatic_drop_imports ||
							assignment.automatic_scan_discovered)
				)
				.map((assignment) => assignment.root_id)
		)
	);
	// Only listed roots count, so the number never exceeds the roots shown.
	const automaticCount = $derived(roots.filter((root) => automaticRootIds.has(root.id)).length);
	const automaticLabel = $derived(
		automaticCount === 0
			? 'Off (manual only)'
			: `On for ${automaticCount} of ${roots.length} ${roots.length === 1 ? 'root' : 'roots'}`
	);
	// The same rule as the Organize tab's Restore button.
	const restoreReady = $derived(
		!recoveryQuery.isError && Boolean(managementSettingsQuery.data) && Boolean(settingsQuery.data)
	);

	interface RecentRow {
		key: string;
		ok: boolean | null;
		result: string;
		task: string;
		scope: string;
		started: number;
		finished: number | null;
		changes: string;
		href: string;
	}

	const terminalOperationStates = ['succeeded', 'failed', 'cancelled', 'stopped'];
	const scanKindLabels: Record<string, string> = {
		incremental: 'Scan for changes',
		rescan_files: 'Rescan every file',
		policy_reconcile: 'Policy check'
	};

	// The last few runs of either kind, newest first, from the lists the page reads.
	const recentRows = $derived.by((): RecentRow[] => {
		const managed = operations
			.filter((item) => item.operation.state !== 'ready' && !item.activation_preview)
			.map((item): RecentRow => {
				const state = item.operation.state;
				const selection = isRecord(item.selection) ? item.selection : {};
				const kind =
					typeof selection.kind === 'string' ? titleManagementValue(selection.kind) : 'Pinned';
				const counts = [
					item.operation.succeeded_count ? `${item.operation.succeeded_count} succeeded` : null,
					item.operation.failed_count ? `${item.operation.failed_count} failed` : null,
					item.operation.skipped_count ? `${item.operation.skipped_count} skipped` : null
				].filter(Boolean);
				return {
					key: `op-${item.operation.id}`,
					ok: state === 'succeeded' ? true : state === 'failed' ? false : null,
					result: titleManagementValue(state),
					task: `Organization (${titleManagementValue(item.mode).toLowerCase()})`,
					scope: `${kind} · ${titleManagementValue(item.origin)}`,
					started: item.operation.created_at,
					finished: terminalOperationStates.includes(state) ? item.operation.updated_at : null,
					changes: counts.join(' · ') || 'No changes',
					href: withBasePath(
						item.mode === 'preview' || item.operation.terminal_code === 'PREVIEW_DISCARDED'
							? `/library/management/previews/${encodeURIComponent(item.operation.id)}`
							: `/library/management/operations/${encodeURIComponent(item.operation.id)}`
					)
				};
			});
		const scans = scanRuns.map((run): RecentRow => ({
			key: `scan-${run.id}`,
			ok: run.state === 'completed' ? true : run.state === 'failed' ? false : null,
			result: titleManagementValue(stateLabel(run.state)),
			task: scanKindLabels[run.kind] ?? titleManagementValue(run.kind),
			scope: run.aggregate_scope === 'all' ? 'All roots' : 'Selected roots',
			started: run.started_at ?? run.queued_at,
			finished: run.terminal_at ?? null,
			changes: `${run.counters.new_count.toLocaleString()} new · ${run.counters.changed_count.toLocaleString()} changed`,
			href: withBasePath('/library/management?tab=scanning#recent-runs')
		}));
		return [...managed, ...scans].sort((a, b) => b.started - a.started).slice(0, 4);
	});

	function stamp(seconds: number | null): string {
		if (!seconds) return '-';
		return new Date(seconds * 1000).toLocaleString(undefined, {
			dateStyle: 'short',
			timeStyle: 'short'
		});
	}

	function duration(timestamp: number | null): string {
		if (!timestamp) return 'just now';
		const seconds = Math.max(0, now - timestamp);
		if (seconds < 60) return 'just now';
		if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
		return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m ago`;
	}

	function timing(item: LibraryWorkItem): string {
		if (item.state === 'failed') return `Failed ${duration(item.failure_at ?? item.updated_at)}`;
		if (item.state === 'paused') return `Paused ${duration(item.updated_at)}`;
		if (item.state === 'queued' || !item.started_at) return 'Waiting to start';
		const seconds = Math.max(0, now - item.started_at);
		if (seconds < 60) return 'Started just now';
		if (seconds < 3600) return `Running for ${Math.floor(seconds / 60)}m`;
		return `Running for ${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
	}

	function workSteps(item: LibraryWorkItem): Array<{ label: string; state: string }> {
		let labels: string[];
		let active = 0;
		if (item.kind === 'scan') {
			labels = ['Find files', 'Read metadata', 'Finalize catalog'];
			active = item.phase === 'reconciling' ? 2 : item.phase === 'indexing' ? 1 : 0;
		} else if (item.kind === 'library_management' && item.effect === 'file_writing') {
			labels = ['Snapshot', 'Stage', 'Validate', 'Publish', 'Catalog', 'Clean up'];
			active =
				{
					preparing_snapshots: 0,
					writing_staged_files: 1,
					validating_staged_files: 2,
					publishing_files: 3,
					committing_catalog: 4,
					cleaning_up: 5
				}[item.phase ?? ''] ?? 0;
		} else if (item.kind === 'library_management') {
			labels = ['Count scope', 'Inspect files', 'Seal preview'];
			active = item.processed > 0 ? 1 : 0;
		} else {
			labels = ['Queue', 'Check evidence', 'Prepare result'];
			active = item.state === 'queued' ? 0 : 1;
		}
		return labels.map((label, index) => ({
			label,
			state: index < active ? 'complete' : index === active ? 'current' : 'pending'
		}));
	}

	function stateLabel(state: string): string {
		const labels: Record<string, string> = {
			idle: 'Idle',
			discovering: 'Counting local files',
			indexing: 'Indexing local files',
			reconciling: 'Reconciling library',
			paused: 'Paused',
			superseded_policy_changed: 'Stopped because library policy changed',
			failed: 'Failed'
		};
		return labels[state] ?? state.replaceAll('_', ' ');
	}

	function startScan(): void {
		void requestRun
			.mutateAsync({
				kind: 'incremental',
				scope_ids: [],
				expected_policy_revision: policyRevision
			})
			.catch(() => undefined);
	}
</script>

<div class="lm-overview">
	<div class="lm-main">
		{#if !libraryEnabled}
			<div class="alert alert-warning">
				<TriangleAlert class="h-5 w-5" />
				<div class="min-w-0 flex-1">
					<strong>The local library is disabled</strong>
					<p class="text-sm">
						Scanning and file organization are paused. Existing catalog data and playback keep
						working. Enable the library in
						<a class="link link-primary" href={withBasePath('/settings?tab=library')}>Settings</a> to
						start new work.
					</p>
				</div>
			</div>
		{/if}
		<section class="lm-now" data-effect={effect} aria-label="Current library work">
			{#if activityQuery.isLoading}
				<div class="skeleton h-28 rounded-lg"></div>
			{:else if activityQuery.isError}
				<div class="alert alert-error">Could not load current work.</div>
			{:else if !primary}
				{#if drainActive}
					<div class="lm-now__row">
						<span class="lm-now__mark"><ScanSearch class="size-7" /></span>
						<div class="min-w-0 flex-1">
							<p class="lm-mono-head">Current work</p>
							<h2 class="lm-now__title">
								Scan complete ({totalTracks.toLocaleString()} files), matching {drainPending.toLocaleString()}
								{drainPending === 1 ? 'album' : 'albums'}
							</h2>
							<p class="lm-now__line">MusicBrainz 1/s limit, playback unaffected.</p>
						</div>
						<Button
							variant="outline"
							size="sm"
							icon={ArrowRight}
							iconPosition="end"
							href={withBasePath('/library/management?tab=scanning#scanning-controls')}
							>View matching progress</Button
						>
					</div>
				{:else}
					<div class="lm-now__row">
						<span class="lm-now__mark"><CircleCheck class="size-8" /></span>
						<div class="min-w-0 flex-1">
							<p class="lm-mono-head">Current work</p>
							<h2 class="lm-now__title">Nothing is running right now</h2>
							<p class="lm-now__line">
								Start a scan or preview file organization below - anything in progress will show up
								here.
							</p>
						</div>
					</div>
				{/if}
			{:else}
				<div class="space-y-4">
					<header class="lm-now__row lm-now__row--top">
						<span class="lm-now__mark lm-now__mark--work"
							><LibraryWorkIcon item={primary} className="size-6" /></span
						>
						<div class="min-w-0 flex-1">
							<p class="lm-mono-head flex items-center gap-1.5">
								<Radio class="h-3.5 w-3.5" />
								{primary.effect === 'attention' ? 'Needs attention' : 'Current work'}
							</p>
							<h2 class="lm-now__title">{libraryWorkTitle(primary)}</h2>
							<div class="lm-now__facts">
								{#if primary.effect !== 'attention'}<span>{libraryWorkEffect(primary)}</span>{/if}
								<span class="flex items-center gap-1"
									><Clock3 class="h-3.5 w-3.5" /> {timing(primary)}</span
								>
								{#if libraryWorkContext(primary)}<span>{libraryWorkContext(primary)}</span>{/if}
								{#if scanActive && isQueuedPreview(primary)}<span>{WAITING_FOR_SCAN_HINT}</span
									>{/if}
							</div>
						</div>
						<Button
							variant="outline"
							size="sm"
							icon={ArrowRight}
							iconPosition="end"
							href={libraryWorkHref(primary)}>Open details</Button
						>
					</header>

					<LibraryWorkProgress item={primary} />

					{#if steps.length}
						<ol class="lm-steps" aria-label="Operation phases">
							{#each steps as step (step.label)}
								<li data-state={step.state}>
									<span class="lm-steps__dot"></span>{step.label}
								</li>
							{/each}
						</ol>
					{/if}

					{#if facts.length}
						<div class="flex flex-wrap gap-1.5" aria-label="Current work details">
							{#each facts as fact (fact)}<span class="lm-fact">{fact}</span>{/each}
						</div>
					{/if}

					{#if additional.length}
						<div class="lm-others">
							<p class="lm-mono-head">
								{additional.length} other {additional.length === 1 ? 'task' : 'tasks'}
							</p>
							{#each additional as item (item.id)}
								<a href={libraryWorkHref(item)} class="lm-others__row">
									<LibraryWorkIcon {item} className="h-4 w-4 text-muted" />
									<span class="min-w-0 flex-1"
										><strong class="font-semibold">{libraryWorkTitle(item)}</strong>
										<span class="text-muted">· {libraryWorkEffect(item)}</span>
										{#if scanActive && isQueuedPreview(item)}<span class="block text-xs text-muted"
												>{WAITING_FOR_SCAN_HINT}</span
											>{/if}</span
									>
									<ArrowRight class="h-4 w-4 text-muted" />
								</a>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		</section>

		<div class="lm-stats stagger-fade-in">
			<Card href={withBasePath('/library/tracks')} variant="torn" padding="none" class="lm-stat">
				<Music2 class="lm-stat__icon" />
				<strong class="lm-stat__value">{totalTracks.toLocaleString()}</strong>
				<span class="lm-stat__label">Tracks</span>
				<ArrowRight class="lm-stat__go" />
			</Card>
			<Card
				href={withBasePath('/library/management?tab=scanning#recent-runs')}
				variant="torn"
				padding="none"
				class="lm-stat"
			>
				<CalendarDays class="lm-stat__icon" />
				<strong class="lm-stat__value whitespace-nowrap"
					>{latestTerminalRun?.terminal_at
						? new Date(latestTerminalRun.terminal_at * 1000).toLocaleDateString()
						: 'Never'}</strong
				>
				<span class="lm-stat__label"
					>Last scan{latestTerminalRun ? ` · ${stateLabel(latestTerminalRun.state)}` : ''}</span
				>
				<ArrowRight class="lm-stat__go" />
			</Card>
			<Card
				href={withBasePath('/library/review')}
				variant="torn"
				padding="none"
				class="lm-stat {reviewCount > 0 ? 'lm-stat--flag' : ''}"
			>
				<ListChecks class="lm-stat__icon" />
				<strong class="lm-stat__value"
					>{reviewCount.toLocaleString()} <span class="lm-stat__unit">Needs you</span></strong
				>
				<span class="lm-stat__note"
					>{guessCount.toLocaleString()} best {guessCount === 1 ? 'guess' : 'guesses'} · {localOnlyCount.toLocaleString()}
					local-only</span
				>
				<ArrowRight class="lm-stat__go" />
			</Card>
			{#if attentionCount > 0}
				<Card
					href={withBasePath('/library/management?tab=organize')}
					variant="torn"
					padding="none"
					class="lm-stat lm-stat--flag"
				>
					<TriangleAlert class="lm-stat__icon" />
					<strong class="lm-stat__value"
						>{attentionCount.toLocaleString()}
						<span class="lm-stat__unit">Needs attention</span></strong
					>
					<ArrowRight class="lm-stat__go" />
				</Card>
			{:else}
				<Card
					href={withBasePath('/library/management?tab=organize')}
					variant="torn"
					padding="none"
					class="lm-stat"
				>
					<Layers class="lm-stat__icon" />
					<strong class="lm-stat__value"
						>{readyPreviewCount.toLocaleString()}
						<span class="lm-stat__unit">Ready previews</span></strong
					>
					<ArrowRight class="lm-stat__go" />
				</Card>
			{/if}
		</div>

		<p class="lm-schedule">
			<CalendarDays class="size-5 flex-none" aria-hidden="true" />
			<span class="lm-schedule__next">{scheduleLead}</span>
			{#if scheduleDetail}<span class="lm-schedule__rule">{scheduleDetail}</span>{/if}
		</p>

		<div class="lm-actions">
			<Card variant="torn" padding="none" class="lm-action">
				<Illustration name="record-crate" class="lm-action__art" />
				<div class="lm-action__body">
					<h3 class="lm-action__title">Scan your library</h3>
					<p class="lm-action__line">
						Checks for new, changed, and missing files and updates the catalog. Never writes to your
						music files.
					</p>
					<Button
						variant="primary"
						size="display-sm"
						icon={RefreshCw}
						disabled={requestRun.isPending || !policyRevision || !libraryEnabled}
						onclick={startScan}>Scan for changes</Button
					>
					<a class="lm-action__link" href={withBasePath('/library/management?tab=scanning')}
						>More scan actions <ArrowRight class="size-4" aria-hidden="true" /></a
					>
				</div>
			</Card>
			<Card variant="torn" padding="none" class="lm-action">
				<Illustration name="tag-boxes" class="lm-action__art" />
				<div class="lm-action__body">
					<h3 class="lm-action__title">Organize files</h3>
					<p class="lm-action__line">
						Preview exactly what would change to tags, names, and paths before anything is written.
					</p>
					<Button
						variant="warning"
						size="display-sm"
						icon={Sparkles}
						href={withBasePath('/library/management?tab=organize&runner=manage')}
						>Preview organization...</Button
					>
					<a class="lm-action__link" href={withBasePath('/library/management?tab=automation')}
						>Automation & profiles <ArrowRight class="size-4" aria-hidden="true" /></a
					>
				</div>
			</Card>
			<Card variant="torn" padding="none" class="lm-action">
				<Illustration name="fingerprint" class="lm-action__art" />
				<div class="lm-action__body">
					<h3 class="lm-action__title">Prepare identities</h3>
					<p class="lm-action__line">
						Organization needs exact MusicBrainz editions and track maps first.
					</p>
					<Button
						variant="paper"
						size="display-sm"
						href={withBasePath('/library/management?tab=organize#identity-readiness')}
						>Open identity readiness</Button
					>
					{#if identityShortfall}
						<a
							class="lm-action__link"
							href={withBasePath('/library/management?tab=organize#identity-readiness')}
							>{identityShortfall.toLocaleString()} albums need preparation
							<ArrowRight class="size-4" aria-hidden="true" /></a
						>
					{/if}
				</div>
			</Card>
		</div>

		{#if recentRows.length}
			<section class="lm-recent" aria-labelledby="overview-recent-work">
				<header class="lm-recent__head">
					<h2 id="overview-recent-work" class="lm-display-head">Recent management work</h2>
					<a class="lm-underline" href={withBasePath('/library/management/history')}
						>View organization history <ArrowUpRight class="size-4" aria-hidden="true" /></a
					>
				</header>
				<div class="lm-recent__scroll">
					<table class="lm-table">
						<thead>
							<tr>
								<th scope="col">Result</th>
								<th scope="col">Task</th>
								<th scope="col">Scope</th>
								<th scope="col">Started</th>
								<th scope="col">Finished</th>
								<th scope="col">Changes</th>
								<th scope="col"><span class="sr-only">Details</span></th>
							</tr>
						</thead>
						<tbody>
							{#each recentRows as row (row.key)}
								<tr>
									<td>
										<span class="lm-result" data-ok={row.ok === null ? undefined : String(row.ok)}
											>{#if row.ok === true}<CircleCheck
													class="size-5"
													aria-hidden="true"
												/>{:else if row.ok === false}<CircleX
													class="size-5"
													aria-hidden="true"
												/>{:else}<CircleDashed class="size-5" aria-hidden="true" />{/if}
											{row.result}</span
										>
									</td>
									<td>{row.task}</td>
									<td>{row.scope}</td>
									<td class="lm-table__mono">{stamp(row.started)}</td>
									<td class="lm-table__mono">{stamp(row.finished)}</td>
									<td>{row.changes}</td>
									<td class="text-right">
										<a class="lm-underline" href={row.href}
											>Details<span class="sr-only">: {row.task}, {stamp(row.started)}</span>
											<ArrowRight class="size-4" aria-hidden="true" /></a
										>
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			</section>
		{/if}
	</div>

	<aside class="lm-side" aria-label="Library at a glance and quick actions">
		<section class="lm-panel" aria-labelledby="overview-glance">
			<h2 id="overview-glance" class="lm-strip">Library at a glance</h2>
			<ul class="lm-glance">
				{#each roots as root (root.id)}
					<li class="lm-glance__row">
						<Folder class="size-6 flex-none" aria-hidden="true" />
						<span class="min-w-0 flex-1">
							<strong>{root.label}</strong>
							<span class="lm-glance__path">{root.path}</span>
						</span>
					</li>
				{:else}
					<li class="lm-glance__row">
						<Folder class="size-6 flex-none" aria-hidden="true" />
						<span class="min-w-0 flex-1">
							<strong>Library root</strong>
							<span class="lm-glance__path">None set up yet</span>
						</span>
					</li>
				{/each}
				<li>
					<a
						class="lm-glance__row lm-glance__row--link"
						href={withBasePath('/library/management?tab=automation')}
					>
						{#if automaticCount > 0}
							<CircleCheck class="size-6 flex-none" aria-hidden="true" />
						{:else}
							<CircleDashed class="size-6 flex-none" aria-hidden="true" />
						{/if}
						<span class="min-w-0 flex-1">
							<strong>Automatic organization</strong>
							<span class="lm-glance__path">{automaticLabel}</span>
						</span>
						<ChevronRight class="size-5 flex-none" aria-hidden="true" />
					</a>
				</li>
				<li>
					<a
						class="lm-glance__row lm-glance__row--link"
						href={withBasePath('/library/management?tab=scanning#scanning-controls')}
					>
						<CalendarDays class="size-6 flex-none" aria-hidden="true" />
						<span class="min-w-0 flex-1">
							<strong>Next scan</strong>
							<span class="lm-glance__path">{scheduleGlance}</span>
						</span>
						<ChevronRight class="size-5 flex-none" aria-hidden="true" />
					</a>
				</li>
			</ul>
		</section>

		<section class="lm-panel lm-panel--pad" aria-labelledby="overview-quick">
			<h2 id="overview-quick" class="lm-display-head">Quick actions</h2>
			<nav class="lm-quick" aria-labelledby="overview-quick">
				<a href={withBasePath('/library/management?tab=automation')}
					><Settings2 class="size-5" aria-hidden="true" />Manage automation...</a
				>
				<a href={withBasePath('/library/management?tab=automation#management-profiles-title')}
					><Layers class="size-5" aria-hidden="true" />Edit organization profiles...</a
				>
				<a href={withBasePath('/library/management?tab=organize#library-repair')}
					><Wrench class="size-5" aria-hidden="true" />Repair & re-check...</a
				>
				{#if restoreReady}
					<a href={withBasePath('/library/management?tab=organize&runner=baseline_restore')}
						><RotateCcw class="size-5" aria-hidden="true" />Restore original state...</a
					>
				{/if}
			</nav>
		</section>

		<Card variant="acid" padding="none" class="lm-how">
			<h2 class="lm-how__title">How it works</h2>
			<ol class="lm-how__steps">
				<li>
					<span class="lm-how__n">1</span><span
						><strong>Scan for changes</strong>Find new, changed or missing files.</span
					>
				</li>
				<li>
					<span class="lm-how__n">2</span><span
						><strong>Review & preview</strong>See exactly what will change (tags, names, paths).</span
					>
				</li>
				<li>
					<span class="lm-how__n">3</span><span
						><strong>Apply</strong>Write the approved changes to your library.</span
					>
				</li>
			</ol>
		</Card>
	</aside>
</div>

<style>
	.lm-overview {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1.5rem;
	}
	.lm-main,
	.lm-side {
		display: grid;
		align-content: start;
		gap: 1.25rem;
		grid-template-columns: minmax(0, 1fr);
		min-width: 0;
	}
	@media (min-width: 1280px) {
		.lm-overview {
			grid-template-columns: minmax(0, 1fr) 21rem;
		}
	}
	@media (min-width: 768px) and (max-width: 1279px) {
		.lm-side {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
		.lm-side :global(.lm-how) {
			grid-column: 1 / -1;
		}
	}

	.lm-mono-head {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.lm-display-head {
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
		color: var(--dn-title);
	}

	/* current work: a bordered dark panel with the acid check */
	.lm-now {
		padding: 1.25rem 1.5rem;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.625rem;
		background-color: var(--color-base-200);
	}
	.lm-now[data-effect='file_writing'] {
		border-color: var(--color-warning);
	}
	.lm-now[data-effect='attention'] {
		border-color: var(--color-error);
	}
	.lm-now__row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 1.25rem;
	}
	.lm-now__row--top {
		align-items: flex-start;
	}
	.lm-now__mark {
		display: grid;
		flex: none;
		place-items: center;
		width: 3.5rem;
		height: 3.5rem;
		color: var(--dn-title);
	}
	.lm-now__mark--work {
		border: 1.5px solid currentColor;
		border-radius: 9999px;
	}
	[data-effect='file_writing'] .lm-now__mark {
		color: var(--color-warning);
	}
	[data-effect='attention'] .lm-now__mark {
		color: var(--color-error);
	}
	.lm-now__title {
		margin-top: 0.25rem;
		font-family: var(--font-display);
		font-size: 2rem;
		line-height: 1.05;
		text-transform: uppercase;
		font-synthesis: none;
		color: var(--color-base-content);
	}
	.lm-now__line {
		margin-top: 0.375rem;
		color: var(--color-base-content);
	}
	.lm-now__facts {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem 1rem;
		margin-top: 0.375rem;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.lm-steps {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem 1.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.lm-steps li {
		display: flex;
		align-items: center;
		gap: 0.375rem;
	}
	.lm-steps li[data-state='current'] {
		font-weight: 700;
		color: var(--color-base-content);
	}
	.lm-steps__dot {
		width: 0.5rem;
		height: 0.5rem;
		border: 1.5px solid currentColor;
		border-radius: 9999px;
	}
	[data-state='current'] .lm-steps__dot {
		border-color: var(--dn-title);
		background-color: var(--dn-title);
	}
	[data-state='complete'] .lm-steps__dot {
		border-color: var(--color-success);
		background-color: var(--color-success);
	}
	.lm-fact {
		padding: 0.25rem 0.625rem;
		border: 1px solid var(--dn-line);
		border-radius: 9999px;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--dn-muted);
	}
	.lm-others {
		display: grid;
		gap: 0.25rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--dn-line);
	}
	.lm-others__row {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		padding: 0.375rem 0.5rem;
		border-radius: 0.375rem;
		font-size: 0.875rem;
	}
	.lm-others__row:hover {
		background-color: var(--dn-selected);
	}

	/* four paper stat tickets, each a link */
	.lm-stats {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 1rem;
	}
	.lm-stats :global(.lm-stat > .card-body) {
		position: relative;
		display: grid;
		grid-template-rows: auto 1fr auto;
		align-content: start;
		gap: 0.25rem;
		min-height: 9rem;
		padding: 1.25rem 3rem 1.25rem 1.25rem;
	}
	.lm-stats :global(.lm-stat__icon) {
		width: 1.75rem;
		height: 1.75rem;
		margin-bottom: 0.5rem;
	}
	.lm-stats :global(.lm-stat__value) {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: clamp(1.75rem, 2.3vw, 2.375rem);
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
		font-variant-numeric: tabular-nums;
	}
	.lm-stats :global(.lm-stat__unit) {
		font-size: 0.8em;
	}
	.lm-stats :global(.lm-stat__label) {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.18em;
		text-transform: uppercase;
	}
	.lm-stats :global(.lm-stat__note) {
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.lm-stats :global(.lm-stat__go) {
		position: absolute;
		right: 1.25rem;
		bottom: 1.5rem;
		width: 1.5rem;
		height: 1.5rem;
	}
	/* an amber corner when something waits on a person */
	.lm-stats :global(.lm-stat--flag) {
		background-image:
			linear-gradient(225deg, var(--color-warning) 0 1.5rem, transparent 1.5rem),
			linear-gradient(45deg, var(--color-warning) 0 0.9rem, transparent 0.9rem);
	}

	.lm-schedule {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.875rem;
		padding: 0.75rem 1.25rem;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.5rem;
		font-family: var(--font-mono);
		color: var(--color-base-content);
	}
	.lm-schedule__next {
		flex: 1 1 auto;
		font-size: 1rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.lm-schedule__rule {
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}

	/* three paper action cards, each with its printed drawing */
	.lm-actions {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 1rem;
	}
	.lm-actions :global(.lm-action > .card-body) {
		display: grid;
		grid-template-columns: 6.5rem minmax(0, 1fr);
		align-items: start;
		gap: 0.75rem;
		height: 100%;
		padding: 1.5rem 1.25rem 1.25rem 0.75rem;
	}
	.lm-actions :global(.lm-action__art) {
		width: 100%;
		max-height: 9rem;
		margin-top: 1rem;
	}
	.lm-action__body {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.75rem;
		height: 100%;
		background: transparent;
		color: var(--dn-paper-content);
	}
	.lm-action__title {
		font-family: var(--font-display);
		font-size: 1.875rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.lm-action__line {
		flex: 1;
		font-size: 0.9375rem;
		line-height: 1.4;
	}
	.lm-action__body :global(.btn) {
		width: 100%;
		height: auto;
		min-height: 2.5rem;
		padding-block: 0.375rem;
		white-space: normal;
	}
	.lm-action__link,
	.lm-underline {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		font-weight: 600;
		text-decoration: underline;
		text-underline-offset: 0.2em;
		white-space: nowrap;
	}
	.lm-action__link {
		align-self: center;
		font-size: 0.9375rem;
	}
	.lm-underline:hover,
	.lm-action__link:hover {
		text-decoration-thickness: 2px;
	}

	/* recent work: a ruled table on a bordered panel */
	.lm-recent {
		padding: 1.25rem;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.625rem;
		background-color: var(--color-base-200);
	}
	.lm-recent__head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 1rem;
	}
	.lm-recent__head .lm-underline {
		font-size: 0.9375rem;
	}
	.lm-recent__scroll {
		overflow-x: auto;
	}
	.lm-table {
		width: 100%;
		min-width: 46rem;
		border-collapse: collapse;
		font-size: 0.9375rem;
	}
	.lm-table th {
		padding: 0.625rem 0.75rem;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-align: left;
		text-transform: uppercase;
	}
	.lm-table th:first-child {
		border-radius: 0.375rem 0 0 0.375rem;
	}
	.lm-table th:last-child {
		border-radius: 0 0.375rem 0.375rem 0;
	}
	.lm-table td {
		padding: 0.625rem 0.625rem;
		border-bottom: 1px solid var(--dn-line);
		vertical-align: middle;
	}
	.lm-table tr:last-child td {
		border-bottom: 0;
	}
	.lm-table__mono {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		white-space: nowrap;
	}
	.lm-result {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		font-weight: 600;
		white-space: nowrap;
	}
	.lm-table :global(svg) {
		flex: none;
		width: 1.25rem;
		height: 1.25rem;
	}
	.lm-table .lm-underline :global(svg) {
		width: 1rem;
		height: 1rem;
	}
	.lm-result[data-ok='true'] {
		color: var(--color-success);
	}
	.lm-result[data-ok='false'] {
		color: var(--color-error);
	}

	/* the side column */
	.lm-panel {
		overflow: hidden;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.625rem;
		background-color: var(--color-base-200);
	}
	.lm-panel--pad {
		display: grid;
		gap: 1rem;
		padding: 1.25rem;
	}
	.lm-strip {
		padding: 0.875rem 1.25rem 1.375rem;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
		clip-path: polygon(
			0 0,
			100% 0,
			100% 58%,
			93% 74%,
			84% 66%,
			72% 82%,
			61% 72%,
			48% 86%,
			36% 76%,
			24% 90%,
			13% 80%,
			0 92%
		);
	}
	/* in light the panel is already paper, so the strip turns forest */
	:global([data-theme='light']) .lm-strip {
		background-color: var(--dn-title);
		color: var(--color-base-100);
	}
	:global([data-theme='light']) .lm-table th {
		background-color: var(--dn-selected);
		color: var(--color-base-content);
	}
	.lm-glance {
		display: grid;
		padding: 0 0.5rem 0.5rem;
	}
	.lm-glance > li + li {
		border-top: 1px solid var(--dn-line);
	}
	.lm-glance__row {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		padding: 0.75rem;
		border-radius: 0.375rem;
	}
	.lm-glance__row strong {
		display: block;
		font-size: 0.9375rem;
	}
	.lm-glance__path {
		display: block;
		font-size: 0.875rem;
		overflow-wrap: anywhere;
		color: var(--dn-muted);
	}
	.lm-glance__row--link:hover {
		background-color: var(--dn-selected);
	}
	.lm-quick {
		display: grid;
		gap: 0.625rem;
	}
	.lm-quick a {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		min-height: 2.75rem;
		padding: 0.5rem 0.875rem;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.375rem;
		font-weight: 600;
		transition: background-color 150ms;
	}
	.lm-quick a:hover {
		background-color: var(--dn-selected);
	}
	.lm-quick a:focus-visible,
	.lm-glance__row--link:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 2px;
	}
	.lm-side :global(.lm-how > .card-body) {
		gap: 1rem;
		padding: 2rem 1.5rem;
	}
	.lm-how__title {
		font-family: var(--font-display);
		font-size: 2rem;
		line-height: 1;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.lm-how__steps {
		display: grid;
		gap: 1rem;
	}
	.lm-how__steps li {
		display: flex;
		align-items: flex-start;
		gap: 0.875rem;
		font-size: 0.875rem;
	}
	.lm-how__steps strong {
		display: block;
		font-size: 1rem;
	}
	.lm-how__n {
		display: grid;
		flex: none;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		border-radius: 9999px;
		background-color: var(--dn-action-content);
		color: var(--dn-action);
		font-family: var(--font-display);
		font-size: 1.25rem;
	}

	@media (max-width: 1535px) {
		.lm-actions :global(.lm-action > .card-body) {
			grid-template-columns: 1fr;
			padding: 1.5rem 1.25rem 1.25rem;
		}
		.lm-actions :global(.lm-action__art) {
			width: 5.5rem;
			height: 5.5rem;
			margin-top: 0;
		}
	}
	@media (max-width: 1023px) {
		.lm-stats {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
		.lm-actions {
			grid-template-columns: minmax(0, 1fr);
		}
		.lm-actions :global(.lm-action > .card-body) {
			grid-template-columns: 6rem minmax(0, 1fr);
		}
	}
	@media (max-width: 767px) {
		.lm-now {
			padding: 1rem;
		}
		.lm-now__row {
			gap: 0.75rem;
		}
		.lm-now__mark {
			width: 2.5rem;
			height: 2.5rem;
		}
		.lm-now__title {
			font-size: 1.5rem;
		}
		.lm-now__line {
			font-size: 0.875rem;
		}
		.lm-stats {
			grid-template-columns: minmax(0, 1fr);
			gap: 0.625rem;
		}
		.lm-stats :global(.lm-stat > .card-body) {
			grid-template-columns: auto minmax(0, 1fr);
			grid-template-rows: auto auto;
			column-gap: 1rem;
			align-items: center;
			min-height: 0;
			padding: 1rem 3rem 1rem 1rem;
		}
		.lm-stats :global(.lm-stat__icon) {
			grid-row: 1 / 3;
			margin: 0;
		}
		.lm-stats :global(.lm-stat__value) {
			font-size: 1.75rem;
		}
		.lm-stats :global(.lm-stat__go) {
			top: 50%;
			bottom: auto;
			translate: 0 -50%;
		}
		.lm-schedule {
			padding: 0.625rem 0.875rem;
		}
		.lm-schedule__next {
			font-size: 0.875rem;
		}
		.lm-actions :global(.lm-action > .card-body) {
			grid-template-columns: 4.5rem minmax(0, 1fr);
			padding: 1.25rem 1rem 1rem;
		}
		.lm-actions :global(.lm-action__art) {
			width: 4.5rem;
			height: 4.5rem;
		}
		.lm-action__title {
			font-size: 1.5rem;
		}
		.lm-display-head {
			font-size: 1.5rem;
		}
	}
	@media (max-width: 399px) {
		.lm-actions :global(.lm-action > .card-body) {
			grid-template-columns: minmax(0, 1fr);
		}
		.lm-actions :global(.lm-action__art) {
			width: 4rem;
			height: 4rem;
		}
	}
</style>
