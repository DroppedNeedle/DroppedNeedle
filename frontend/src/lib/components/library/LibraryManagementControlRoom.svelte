<script lang="ts">
	import { onMount } from 'svelte';
	import {
		AlertTriangle,
		ArrowRight,
		CirclePause,
		CirclePlay,
		FolderCog,
		History,
		RotateCcw,
		Settings2,
		ShieldAlert,
		Sparkles
	} from 'lucide-svelte';

	import LibraryManagementRunner from './LibraryManagementRunner.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { createLibraryManagementEvents } from '$lib/queries/library-management/LibraryManagementEvents';
	import { controlLibraryManagementOperationMutation } from '$lib/queries/library-management/LibraryManagementMutations.svelte';
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
	let runnerMode = $state<'manage' | 'baseline_restore' | null>(null);
	let runnerOpener = $state<HTMLButtonElement | null>(null);

	const history = $derived(operationsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const active = $derived(
		history.find((item) => ['queued', 'running', 'paused'].includes(item.operation.state)) ?? null
	);
	const readyPreviews = $derived(
		history.filter((item) => item.operation.state === 'ready').slice(0, 3)
	);
	const recent = $derived(history.filter((item) => item.operation.state !== 'ready').slice(0, 5));
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
			history.filter((item) => item.operation.state === 'failed').length
	);
	const recoveryUnavailable = $derived(recoveryQuery.isError);

	onMount(() => {
		const events = createLibraryManagementEvents();
		events.start();
		return events.stop;
	});

	function openRunner(mode: 'manage' | 'baseline_restore', opener: HTMLButtonElement): void {
		runnerOpener = opener;
		runnerMode = mode;
	}

	function closeRunner(): void {
		runnerMode = null;
		runnerOpener?.focus();
	}

	function title(value: string): string {
		return value.replaceAll('_', ' ').replace(/^\w/, (letter) => letter.toUpperCase());
	}

	function operationHref(jobId: string, state: string): string {
		return state === 'ready'
			? `/library/management/previews/${encodeURIComponent(jobId)}`
			: `/library/management/operations/${encodeURIComponent(jobId)}`;
	}

	function date(value: number): string {
		return new Date(value * 1000).toLocaleString();
	}
</script>

<section class="management-control-room" aria-labelledby="management-control-title">
	<header class="management-control-header">
		<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
		<div class="min-w-0 flex-1">
			<p class="management-kicker"><ShieldAlert class="h-3.5 w-3.5" /> Separate write system</p>
			<h2 id="management-control-title" class="font-display text-xl font-semibold">
				Library Management
			</h2>
			<p class="mt-1 text-sm text-base-content/60">
				Writes tags and organizes files. Scanning and identification above remain read-only.
			</p>
		</div>
		<a href="/settings?tab=library" class="btn btn-ghost btn-sm"
			><Settings2 class="h-4 w-4" /> Settings</a
		>
	</header>

	{#if settingsQuery.isLoading || policyQuery.isLoading || operationsQuery.isLoading || recoveryQuery.isLoading}
		<div class="space-y-3 p-5">
			<div class="skeleton h-28 rounded-xl"></div>
			<div class="skeleton h-44 rounded-xl"></div>
		</div>
	{:else if settingsQuery.isError || policyQuery.isError || operationsQuery.isError}
		<div class="m-5 alert alert-error">Could not load Library Management control state.</div>
	{:else if settingsQuery.data && policyQuery.data}
		<div class="space-y-5 p-5 sm:p-6">
			<div class="grid gap-3 sm:grid-cols-3">
				<div class="management-control-stat">
					<span>Automatic write access</span><strong
						>{activeAssignments.length
							? `${activeAssignments.length} active root${activeAssignments.length === 1 ? '' : 's'}`
							: 'Off everywhere'}</strong
					><small>Manual previews remain available while automatic management is off.</small>
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

			{#if active}
				<article class="management-active-card">
					<div class="flex min-w-0 flex-1 items-start gap-3">
						<span class="management-live-dot" aria-hidden="true"></span>
						<div class="min-w-0">
							<p class="management-step">Active write work</p>
							<h3 class="font-semibold">{active.profile_name}</h3>
							<p class="text-sm text-base-content/55">
								{title(active.mode)} · {title(active.phase)} · {active.operation.completed_count.toLocaleString()}
								/ {active.operation.expected_work_count.toLocaleString()}
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
							href={operationHref(active.operation.id, active.operation.state)}
							>Open details <ArrowRight class="h-4 w-4" /></a
						>
					</div>
				</article>
			{/if}

			<div class="flex flex-wrap gap-2">
				<button
					class="btn management-btn"
					disabled={recoveryUnavailable}
					onclick={(event) => openRunner('manage', event.currentTarget)}
					><Sparkles class="h-4 w-4" /> Preview library management...</button
				>
				<button
					class="btn btn-outline"
					disabled={recoveryUnavailable}
					onclick={(event) => openRunner('baseline_restore', event.currentTarget)}
					><RotateCcw class="h-4 w-4" /> Restore first-management state...</button
				>
			</div>

			{#if readyPreviews.length}
				<section class="space-y-2" aria-labelledby="ready-management-previews">
					<div class="flex items-end justify-between">
						<div>
							<p class="management-step">Awaiting review</p>
							<h3 id="ready-management-previews" class="font-semibold">Ready previews</h3>
						</div>
						<span class="text-xs text-base-content/45">Read-only until Apply</span>
					</div>
					{#each readyPreviews as item (item.operation.id)}<a
							href={operationHref(item.operation.id, item.operation.state)}
							class="management-history-row"
							><Sparkles class="h-4 w-4 text-library-manage" /><span class="min-w-0 flex-1"
								><strong>{item.profile_name}</strong><small
									>{title(item.mode)} · {date(item.operation.updated_at)}</small
								></span
							><span class="badge badge-outline badge-sm">Review</span><ArrowRight
								class="h-4 w-4"
							/></a
						>{/each}
				</section>
			{/if}

			<section class="space-y-2" aria-labelledby="recent-management-work">
				<div class="flex items-end justify-between">
					<div>
						<p class="management-step">Durable audit trail</p>
						<h3 id="recent-management-work" class="font-semibold">Recent management work</h3>
					</div>
					<a class="link text-xs" href="/library/management/history">All history</a>
				</div>
				{#if recent.length}{#each recent as item (item.operation.id)}<a
							href={operationHref(item.operation.id, item.operation.state)}
							class="management-history-row"
							><History class="h-4 w-4 text-base-content/45" /><span class="min-w-0 flex-1"
								><strong>{item.profile_name}</strong><small
									>{title(item.mode)} · {title(item.operation.state)} · {date(
										item.operation.updated_at
									)}</small
								></span
							>{#if item.operation.failed_count}<span class="badge badge-error badge-sm"
									>{item.operation.failed_count} failed</span
								>{/if}<ArrowRight class="h-4 w-4" /></a
						>{/each}{:else}<div
						class="rounded-xl border border-dashed border-base-content/15 p-4 text-sm text-base-content/45"
					>
						No Library Management work has run yet.
					</div>{/if}
			</section>

			{#if recoveryQuery.data && (recoveryQuery.data.needs_attention_count || recoveryQuery.data.cleanup_pending_count)}<div
					class="alert alert-warning items-start"
				>
					<AlertTriangle class="mt-0.5 h-5 w-5" /><span
						><strong>Recovery needs attention</strong><br />{recoveryQuery.data
							.needs_attention_count} bundles need review; {recoveryQuery.data
							.cleanup_pending_count} have safe cleanup pending. No uncertain file is deleted automatically.</span
					>
				</div>{:else if recoveryUnavailable}<div class="alert alert-error items-start" role="alert">
					<AlertTriangle class="mt-0.5 h-5 w-5" /><span
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
