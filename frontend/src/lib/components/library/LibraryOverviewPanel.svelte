<script lang="ts">
	import {
		AlertTriangle,
		ArrowRight,
		CircleCheck,
		Clock3,
		FolderCog,
		Fingerprint,
		History,
		ListChecks,
		Music2,
		Radio,
		RefreshCw,
		ScanSearch,
		Sparkles
	} from 'lucide-svelte';
	import { onMount } from 'svelte';

	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryActivityQuery } from '$lib/queries/library/LibraryActivityQueries.svelte';
	import { getLibraryRunHistoryQuery } from '$lib/queries/library/LibraryOperationQueries.svelte';
	import { requestLibraryRun } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { getLibraryReviewsQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import {
		getLibraryScanScheduleQuery,
		getLibraryStatsQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import {
		getLibraryManagementOperationsQuery,
		getLibraryManagementRecoveryQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import { getLibraryIdentityPreparationEstimateQuery } from '$lib/queries/library/LibraryIdentityPreparationQueries.svelte';
	import type { LibraryWorkItem } from '$lib/queries/library/LibraryOperationsTypes';
	import { withBasePath } from '$lib/utils/basePath';
	import LibraryWorkIcon from './LibraryWorkIcon.svelte';
	import LibraryWorkProgress from './LibraryWorkProgress.svelte';
	import {
		libraryWorkContext,
		libraryWorkEffect,
		libraryWorkFacts,
		libraryWorkHref,
		libraryWorkTitle
	} from './LibraryWorkPresentation';

	const activityQuery = getLibraryActivityQuery(() => authStore.user?.id);
	const statsQuery = getLibraryStatsQuery();
	const historyQuery = getLibraryRunHistoryQuery(() => authStore.isAdmin);
	const scheduleQuery = getLibraryScanScheduleQuery(() => authStore.isAdmin);
	const reviewsQuery = getLibraryReviewsQuery(() => ({ state: 'needs_review' }));
	const settingsQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
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
	const requestRun = requestLibraryRun();

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
	const drainActive = $derived(
		Boolean(drain?.synthetic) && (drainPending ?? 0) > 0
	);
	const primary = $derived(items[0] ?? null);
	const additional = $derived(items.slice(1));
	const facts = $derived(primary ? libraryWorkFacts(primary) : []);
	const steps = $derived(primary && primary.effect !== 'attention' ? workSteps(primary) : []);
	const effect = $derived(primary?.effect ?? 'idle');

	const heroTint = $derived(
		effect === 'file_writing'
			? 'border-warning/25 bg-gradient-to-br from-warning/10 via-base-200/40 to-base-200/40'
			: effect === 'attention'
				? 'border-error/25 bg-gradient-to-br from-error/10 via-base-200/40 to-base-200/40'
				: 'border-base-content/10 bg-gradient-to-br from-primary/10 via-base-200/40 to-base-200/40'
	);
	const chipTint = $derived(
		effect === 'file_writing'
			? 'bg-warning/15 text-warning'
			: effect === 'attention'
				? 'bg-error/15 text-error'
				: 'bg-primary/10 text-primary'
	);

	const policyRevision = $derived(settingsQuery.data?.policy_revision ?? '');
	const libraryEnabled = $derived(settingsQuery.data?.enabled ?? true);
	const totalTracks = $derived(statsQuery.data?.total_tracks ?? 0);
	const localOnlyCount = $derived(statsQuery.data?.local_only_count ?? 0);
	const latestTerminalRun = $derived(historyQuery.data?.pages[0]?.items[0] ?? null);
	const reviewCount = $derived(reviewsQuery.data?.pages[0]?.filtered_total ?? 0);
	const readyPreviewCount = $derived(
		(operationsQuery.data?.pages.flatMap((result) => result.items) ?? []).filter(
			(item) => item.operation.state === 'ready' && !item.activation_preview
		).length
	);
	const attentionCount = $derived(
		(recoveryQuery.data?.needs_attention_count ?? 0) +
			(recoveryQuery.data?.cleanup_pending_count ?? 0)
	);
	const identityShortfall = $derived(
		(identityEstimateQuery.data?.mapping_required_count ?? 0) +
			(identityEstimateQuery.data?.exact_release_required_count ?? 0)
	);
	const scheduleText = $derived(
		scheduleQuery.data?.scan_frequency === 'daily'
			? `Next scan: ${scheduleQuery.data.daily_scan_time} ${scheduleQuery.data.server_timezone ?? ''}`
			: scheduleQuery.data?.scan_frequency === 'manual'
				? 'Automatic scanning off'
				: `Schedule: ${scheduleQuery.data?.scan_frequency?.replace('_', ' ') ?? 'loading'}`
	);

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

<div class="space-y-6">
	{#if !libraryEnabled}
		<div class="alert alert-warning">
			<AlertTriangle class="h-5 w-5" />
			<div class="min-w-0 flex-1">
				<strong>The local library is disabled</strong>
				<p class="text-sm">
					Scanning and file organization are paused. Existing catalog data and playback keep
					working. Enable the library in
					<a class="link link-primary" href={withBasePath('/settings?tab=library')}>Settings</a> to start
					new work.
				</p>
			</div>
		</div>
	{/if}
	<section
		class="relative overflow-hidden rounded-3xl border shadow-lg {heroTint}"
		data-effect={effect}
		aria-label="Current library work"
	>
		{#if activityQuery.isLoading}
			<div class="skeleton h-40 rounded-3xl"></div>
		{:else if activityQuery.isError}
			<div class="alert alert-error m-5 sm:m-6">Could not load current work.</div>
		{:else if !primary}
			{#if drainActive}
				<div class="flex flex-wrap items-center gap-4 p-5 sm:p-6">
					<span
						class="flex h-11 w-11 flex-none items-center justify-center rounded-2xl bg-primary/10 text-primary"
					>
						<ScanSearch class="h-5 w-5" />
					</span>
					<div class="min-w-0 flex-1">
						<p
							class="font-mono text-[0.65rem] font-semibold tracking-widest text-primary/70 uppercase"
						>
							Current work
						</p>
						<h2 class="font-display mt-1 text-2xl font-bold">
							Scan complete ({totalTracks.toLocaleString()} files) — Matching {drainPending.toLocaleString()}
							{drainPending === 1 ? 'album' : 'albums'}
						</h2>
						<p class="mt-1 text-sm text-base-content/60">
							MusicBrainz 1/s limit, playback unaffected.
						</p>
					</div>
					<a
						class="btn btn-ghost btn-sm"
						href={withBasePath('/library/management?tab=scanning#scanning-controls')}
					>
						View matching progress <ArrowRight class="h-4 w-4" />
					</a>
				</div>
			{:else}
				<div class="flex flex-wrap items-center gap-4 p-5 sm:p-6">
					<span
						class="flex h-11 w-11 flex-none items-center justify-center rounded-2xl bg-primary/10 text-primary"
					>
						<CircleCheck class="h-5 w-5" />
					</span>
					<div class="min-w-0 flex-1">
						<p
							class="font-mono text-[0.65rem] font-semibold tracking-widest text-primary/70 uppercase"
						>
							Current work
						</p>
						<h2 class="font-display mt-1 text-2xl font-bold">Nothing is running right now</h2>
						<p class="mt-1 text-sm text-base-content/60">
							Start a scan or preview file organization below - anything in progress will show up
							here.
						</p>
					</div>
				</div>
			{/if}
		{:else}
			<div class="space-y-4 p-5 sm:p-6">
				<header class="flex flex-wrap items-start gap-4">
					<span class="flex h-11 w-11 flex-none items-center justify-center rounded-2xl {chipTint}">
						<LibraryWorkIcon item={primary} />
					</span>
					<div class="min-w-0 flex-1">
						<p
							class="flex items-center gap-1.5 font-mono text-[0.65rem] font-semibold tracking-widest text-primary/70 uppercase"
						>
							<Radio class="h-3 w-3" />
							{primary.effect === 'attention' ? 'Needs attention' : 'Current work'}
						</p>
						<h2 class="font-display mt-1 text-2xl font-bold">{libraryWorkTitle(primary)}</h2>
						<div class="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-sm text-base-content/60">
							{#if primary.effect !== 'attention'}<span>{libraryWorkEffect(primary)}</span>{/if}
							<span class="flex items-center gap-1"
								><Clock3 class="h-3.5 w-3.5" /> {timing(primary)}</span
							>
							{#if libraryWorkContext(primary)}<span>{libraryWorkContext(primary)}</span>{/if}
						</div>
					</div>
					<a class="btn btn-ghost btn-sm" href={libraryWorkHref(primary)}>
						Open details <ArrowRight class="h-4 w-4" />
					</a>
				</header>

				<LibraryWorkProgress item={primary} />

				{#if steps.length}
					<ol class="flex flex-wrap gap-x-5 gap-y-2" aria-label="Operation phases">
						{#each steps as step (step.label)}
							<li
								class="flex items-center gap-1.5 text-xs font-medium {step.state === 'current'
									? 'text-base-content'
									: step.state === 'complete'
										? 'text-base-content/55'
										: 'text-base-content/40'}"
							>
								<span
									class="h-1.5 w-1.5 rounded-full {step.state === 'current'
										? 'bg-primary'
										: step.state === 'complete'
											? 'bg-success'
											: 'bg-base-content/20'}"
								></span>{step.label}
							</li>
						{/each}
					</ol>
				{/if}

				{#if facts.length}
					<div class="flex flex-wrap gap-1.5" aria-label="Current work details">
						{#each facts as fact (fact)}<span
								class="rounded-full border border-base-content/10 bg-base-100/60 px-2.5 py-1 text-xs text-base-content/70"
								>{fact}</span
							>{/each}
					</div>
				{/if}

				{#if additional.length}
					<div class="space-y-1.5 border-t border-base-content/10 pt-3">
						<p class="text-xs font-semibold tracking-widest text-base-content/50 uppercase">
							{additional.length} other {additional.length === 1 ? 'task' : 'tasks'}
						</p>
						{#each additional as item (item.id)}
							<a
								href={libraryWorkHref(item)}
								class="flex items-center gap-2.5 rounded-xl px-2 py-1.5 text-sm transition-colors hover:bg-base-100/70"
							>
								<LibraryWorkIcon {item} className="h-4 w-4 text-base-content/60" />
								<span class="min-w-0 flex-1"
									><strong class="font-semibold">{libraryWorkTitle(item)}</strong>
									<span class="text-base-content/50">· {libraryWorkEffect(item)}</span></span
								>
								<ArrowRight class="h-4 w-4 text-base-content/40" />
							</a>
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</section>

	<div class="stagger-fade-in grid grid-cols-2 gap-3 lg:grid-cols-4">
		<a
			href={withBasePath('/library/tracks')}
			class="rounded-2xl border border-base-content/10 bg-base-200/40 p-4 transition-colors hover:border-base-content/20 hover:bg-base-200/60"
		>
			<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-primary/10 text-primary"
				><Music2 class="h-4 w-4" /></span
			>
			<strong class="font-display mt-3 block text-2xl font-bold tabular-nums"
				>{totalTracks.toLocaleString()}</strong
			>
			<span
				class="mt-1 block text-xs font-semibold tracking-[0.15em] text-base-content/55 uppercase"
				>Tracks</span
			>
		</a>
		<a
			href={withBasePath('/library/management?tab=scanning#recent-runs')}
			class="rounded-2xl border border-base-content/10 bg-base-200/40 p-4 transition-colors hover:border-base-content/20 hover:bg-base-200/60"
		>
			<span
				class="flex h-9 w-9 items-center justify-center rounded-xl bg-base-content/5 text-base-content/60"
				><History class="h-4 w-4" /></span
			>
			<strong class="font-display mt-3 block text-2xl font-bold tabular-nums"
				>{latestTerminalRun?.terminal_at
					? new Date(latestTerminalRun.terminal_at * 1000).toLocaleDateString()
					: 'Never'}</strong
			>
			<span
				class="mt-1 block text-xs font-semibold tracking-[0.15em] text-base-content/55 uppercase"
				>Last scan{latestTerminalRun ? ` · ${stateLabel(latestTerminalRun.state)}` : ''}</span
			>
		</a>
		<a
			href={withBasePath('/library/review')}
			class="rounded-2xl border border-base-content/10 bg-base-200/40 p-4 transition-colors hover:border-base-content/20 hover:bg-base-200/60"
		>
			<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-accent/10 text-accent"
				><ListChecks class="h-4 w-4" /></span
			>
			<strong class="font-display mt-3 block text-2xl font-bold tabular-nums"
				>{reviewCount.toLocaleString()}</strong
			>
			<span
				class="mt-1 block text-xs font-semibold tracking-[0.15em] text-base-content/55 uppercase"
				>Needs review</span
			>
			<span class="mt-0.5 block text-xs text-base-content/45"
				>{localOnlyCount.toLocaleString()} local-only</span
			>
		</a>
		{#if attentionCount > 0}
			<a
				href={withBasePath('/library/management?tab=organize')}
				class="rounded-2xl border border-warning/30 bg-base-200/40 p-4 transition-colors hover:border-warning/50 hover:bg-base-200/60"
			>
				<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-warning/15 text-warning"
					><AlertTriangle class="h-4 w-4" /></span
				>
				<strong class="font-display mt-3 block text-2xl font-bold text-warning tabular-nums"
					>{attentionCount.toLocaleString()}</strong
				>
				<span
					class="mt-1 block text-xs font-semibold tracking-[0.15em] text-base-content/55 uppercase"
					>Needs attention</span
				>
			</a>
		{:else}
			<a
				href={withBasePath('/library/management?tab=organize')}
				class="rounded-2xl border border-base-content/10 bg-base-200/40 p-4 transition-colors hover:border-base-content/20 hover:bg-base-200/60"
			>
				<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-warning/10 text-warning"
					><Sparkles class="h-4 w-4" /></span
				>
				<strong class="font-display mt-3 block text-2xl font-bold tabular-nums"
					>{readyPreviewCount.toLocaleString()}</strong
				>
				<span
					class="mt-1 block text-xs font-semibold tracking-[0.15em] text-base-content/55 uppercase"
					>Ready previews</span
				>
			</a>
		{/if}
	</div>

	<div class="grid gap-3 md:grid-cols-3">
		<div
			class="flex flex-col gap-3 rounded-2xl border border-primary/15 bg-gradient-to-br from-primary/8 via-base-200/50 to-base-200/40 p-5"
		>
			<div class="flex items-center gap-2.5">
				<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-primary/10 text-primary"
					><ScanSearch class="h-4 w-4" /></span
				>
				<h3 class="font-display text-lg font-bold">Scan your library</h3>
			</div>
			<p class="text-sm text-base-content/60">
				Checks for new, changed, and missing files and updates the catalog. Never writes to your
				music files.
			</p>
			<div class="mt-auto flex flex-col gap-2">
				<button
					class="btn btn-primary rounded-full shadow-lg shadow-primary/25"
					disabled={requestRun.isPending || !policyRevision || !libraryEnabled}
					onclick={startScan}
				>
					<RefreshCw class="h-4 w-4" /> Scan for changes
				</button>
				<div class="flex items-center justify-between gap-2 text-xs text-base-content/55">
					<span>{scheduleText}</span>
					<a
						class="link-hover font-semibold text-primary"
						href={withBasePath('/library/management?tab=scanning')}>More scan actions →</a
					>
				</div>
			</div>
		</div>
		<div
			class="flex flex-col gap-3 rounded-2xl border border-warning/15 bg-gradient-to-br from-warning/8 via-base-200/50 to-base-200/40 p-5"
		>
			<div class="flex items-center gap-2.5">
				<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-warning/10 text-warning"
					><FolderCog class="h-4 w-4" /></span
				>
				<h3 class="font-display text-lg font-bold">Organize files</h3>
			</div>
			<p class="text-sm text-base-content/60">
				Preview exactly what would change to tags, names, and paths before anything is written.
			</p>
			<div class="mt-auto flex flex-col gap-2">
				<a
					class="btn management-btn"
					href={withBasePath('/library/management?tab=organize&runner=manage')}
					><Sparkles class="h-4 w-4" /> Preview organization...</a
				>
				<div class="flex items-center justify-end gap-2 text-xs text-base-content/55">
					<a
						class="link-hover font-semibold text-warning"
						href={withBasePath('/library/management?tab=automation')}>Automation & profiles →</a
					>
				</div>
			</div>
		</div>
		<div
			class="flex flex-col gap-3 rounded-2xl border border-accent/15 bg-gradient-to-br from-accent/8 via-base-200/50 to-base-200/40 p-5"
		>
			<div class="flex items-center gap-2.5">
				<span class="flex h-9 w-9 items-center justify-center rounded-xl bg-accent/10 text-accent"
					><Fingerprint class="h-4 w-4" /></span
				>
				<h3 class="font-display text-lg font-bold">Prepare identities</h3>
			</div>
			<p class="text-sm text-base-content/60">
				Organization needs exact MusicBrainz editions and track maps first.{#if identityShortfall}
					<strong class="text-base-content"
						>{identityShortfall.toLocaleString()} albums need preparation.</strong
					>{/if}
			</p>
			<div class="mt-auto flex flex-col gap-2">
				<a
					class="btn btn-outline"
					href={withBasePath('/library/management?tab=organize#identity-readiness')}
					>Open identity readiness</a
				>
			</div>
		</div>
	</div>
</div>
