<script lang="ts">
	import { CirclePause, CirclePlay, OctagonX, ShieldCheck, Wrench } from 'lucide-svelte';
	import {
		getLibraryRepairEstimateQuery,
		getLibraryRepairFindingsQuery,
		getLibraryRepairsQuery
	} from '$lib/queries/library/LibraryRepairQueries.svelte';
	import { getLibraryPolicyTreeQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { controlLibraryOperation } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import {
		applyLibraryRepair,
		createLibraryRepair
	} from '$lib/queries/library/LibraryRepairMutations.svelte';
	import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';

	const DEFAULT_VISIBLE_REPAIRS = 3;
	const repairsQuery = getLibraryRepairsQuery();
	const policyTreeQuery = getLibraryPolicyTreeQuery();
	const createRepair = createLibraryRepair();
	const applyRepair = applyLibraryRepair();
	let startDialog: HTMLDialogElement;
	let applyDialog: HTMLDialogElement;
	let startHeading: HTMLHeadingElement;
	let applyHeading: HTMLHeadingElement;
	let selected = $state<OperationResponse | null>(null);
	let startOpen = $state(false);
	let showOlder = $state(false);
	let scopeMode = $state<'all' | 'selected'>('all');
	let selectedRootIds = $state<string[]>([]);
	let activeTab = $state<
		'valid' | 'safe_detach' | 'needs_review' | 'unverifiable' | 'manual_identity'
	>('safe_detach');
	const repairs = $derived(repairsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const visibleRepairs = $derived(showOlder ? repairs : repairs.slice(0, DEFAULT_VISIBLE_REPAIRS));
	const olderRepairCount = $derived(Math.max(0, repairs.length - DEFAULT_VISIBLE_REPAIRS));
	const roots = $derived(policyTreeQuery.data?.roots ?? []);
	const estimateRootIds = $derived(scopeMode === 'all' ? [] : selectedRootIds);
	const estimateQuery = getLibraryRepairEstimateQuery(
		() => estimateRootIds,
		() => startOpen && (scopeMode === 'all' || selectedRootIds.length > 0)
	);
	const findingsQuery = getLibraryRepairFindingsQuery(
		() => selected?.id ?? null,
		() => activeTab
	);
	const pause = controlLibraryOperation('pause');
	const resume = controlLibraryOperation('resume');
	const stop = controlLibraryOperation('stop');
	const tabs = [
		{ id: 'valid', label: 'Valid' },
		{ id: 'safe_detach', label: 'Safe to detach' },
		{ id: 'needs_review', label: 'Needs review' },
		{ id: 'unverifiable', label: 'Could not verify' },
		{ id: 'manual_identity', label: 'Manual identities' }
	] as const;
	const findings = $derived(findingsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const rootLabels = $derived(Object.fromEntries(roots.map((root) => [root.id, root.label])));

	function openStart(): void {
		startOpen = true;
		scopeMode = 'all';
		selectedRootIds = [];
		startDialog.showModal();
		startHeading.focus();
	}

	function chooseSelectedRoots(): void {
		scopeMode = 'selected';
		if (selectedRootIds.length === 0) {
			selectedRootIds = roots.filter((root) => root.available).map((root) => root.id);
		}
	}

	function toggleRoot(rootId: string, checked: boolean): void {
		selectedRootIds = checked
			? [...selectedRootIds, rootId]
			: selectedRootIds.filter((id) => id !== rootId);
	}

	function tabCount(repair: OperationResponse, tab: (typeof tabs)[number]['id']): number {
		const counts = repair.repair_summary?.counts_by_finding ?? {};
		return tab === 'unverifiable'
			? (counts.unverifiable ?? 0) + (counts.stale ?? 0)
			: (counts[tab] ?? 0);
	}

	function openReport(repair: OperationResponse): void {
		selected = repair;
		activeTab = tabCount(repair, 'safe_detach') > 0 ? 'safe_detach' : 'valid';
	}

	function openApply(repair: OperationResponse): void {
		selected = repair;
		applyDialog.showModal();
		applyHeading.focus();
	}

	async function startCheck(): Promise<void> {
		try {
			await createRepair.mutateAsync(estimateRootIds);
		} catch {
			return;
		}
		startDialog.close();
	}

	async function applySelected(): Promise<void> {
		if (!selected) return;
		try {
			await applyRepair.mutateAsync({
				jobId: selected.id,
				expectedRevision: selected.row_revision
			});
		} catch {
			return;
		}
		applyDialog.close();
	}

	function visibleState(repair: OperationResponse): string {
		if (repair.control_request === 'stop') return 'stopping';
		if (repair.control_request === 'pause') return 'pausing';
		return repair.state;
	}
</script>

<section
	class="rounded-box border border-base-content/10 bg-base-100"
	aria-labelledby="repair-title"
>
	<div class="flex flex-wrap items-center gap-3 p-4">
		<div class="flex h-9 w-9 items-center justify-center rounded-xl bg-warning/10 text-warning">
			<Wrench class="h-4 w-4" aria-hidden="true" />
		</div>
		<div class="min-w-0 flex-1">
			<h3 id="repair-title" class="font-semibold">Repair</h3>
			<p class="text-sm text-base-content/55">
				Check identities created before the safer matching rules.
			</p>
		</div>
		<button class="btn btn-outline btn-sm" onclick={openStart}>Check existing matches</button>
	</div>

	{#if repairsQuery.isLoading}
		<div class="border-t border-base-content/10 p-4"><div class="skeleton h-12"></div></div>
	{:else if repairs.length > 0}
		<div class="divide-y divide-base-content/10 border-t border-base-content/10">
			{#each visibleRepairs as repair (repair.id)}
				<div class="grid gap-3 p-4 sm:grid-cols-[1fr_auto] sm:items-center">
					<div>
						<div class="flex flex-wrap items-center gap-2">
							<strong>Existing-match check</strong>
							<span class="badge badge-ghost badge-sm">{visibleState(repair)}</span>
						</div>
						<p class="mt-1 text-sm text-base-content/60">
							{repair.completed_count.toLocaleString()} of {repair.expected_work_count.toLocaleString()}
							checked · {repair.failed_count.toLocaleString()} failed · {repair.skipped_count.toLocaleString()}
							stale
						</p>
						{#if repair.repair_summary}
							<p class="mt-1 text-xs text-base-content/50">
								{repair.repair_summary.estimated_apply_changes.toLocaleString()} safe to detach · {repair.repair_summary.remaining_identities.toLocaleString()}
								identities remain
							</p>
						{/if}
					</div>
					<div class="flex flex-wrap justify-end gap-1">
						{#if repair.control_request === 'stop'}<button class="btn btn-ghost btn-xs" disabled
								><span class="loading loading-spinner loading-xs"></span> Stopping...</button
							>{:else if repair.control_request === 'pause'}<button
								class="btn btn-ghost btn-xs"
								disabled><span class="loading loading-spinner loading-xs"></span> Pausing...</button
							>{:else if repair.state === 'running'}<button
								class="btn btn-ghost btn-xs"
								onclick={() =>
									void pause
										.mutateAsync({
											jobId: repair.id,
											expectedRevision: repair.row_revision
										})
										.catch(() => undefined)}><CirclePause class="h-3.5 w-3.5" /> Pause</button
							>{:else if repair.state === 'paused'}<button
								class="btn btn-ghost btn-xs"
								onclick={() =>
									void resume
										.mutateAsync({
											jobId: repair.id,
											expectedRevision: repair.row_revision
										})
										.catch(() => undefined)}><CirclePlay class="h-3.5 w-3.5" /> Resume</button
							>{/if}
						{#if repair.control_request === 'none' && ['queued', 'running', 'paused'].includes(repair.state)}<button
								class="btn btn-ghost btn-xs text-error"
								onclick={() =>
									void stop
										.mutateAsync({
											jobId: repair.id,
											expectedRevision: repair.row_revision
										})
										.catch(() => undefined)}><OctagonX class="h-3.5 w-3.5" /> Stop</button
							>{/if}
						{#if repair.repair_summary}<button
								class="btn btn-outline btn-xs"
								onclick={() => openReport(repair)}>View report</button
							>{/if}
						{#if repair.state === 'ready' && repair.repair_summary?.estimated_apply_changes}<button
								class="btn btn-warning btn-xs"
								onclick={() => openApply(repair)}>Apply safe repairs...</button
							>{/if}
					</div>
					{#if repair.state === 'succeeded' && repair.repair_summary?.estimated_apply_changes}
						<a
							class="text-sm link link-primary sm:col-span-2 sm:justify-self-end"
							href="/library/review?state=needs_review&reason=LEGACY_IDENTITY_FAILED_SAFETY_RULES"
							>Review detached albums</a
						>
					{/if}
				</div>
			{/each}
			{#if olderRepairCount > 0}
				<div class="flex justify-center p-2">
					<button class="btn btn-ghost btn-xs" onclick={() => (showOlder = !showOlder)}>
						{showOlder
							? `Show latest ${DEFAULT_VISIBLE_REPAIRS}`
							: `Show ${olderRepairCount} older ${olderRepairCount === 1 ? 'check' : 'checks'}`}
					</button>
				</div>
			{/if}
		</div>
	{/if}

	{#if selected?.repair_summary}
		<section class="border-t border-base-content/10 p-4" aria-labelledby="repair-report-title">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<h4 id="repair-report-title" class="font-semibold">Existing-match report</h4>
				<button class="btn btn-ghost btn-xs" onclick={() => (selected = null)}>Close report</button>
			</div>
			<dl class="mt-3 grid gap-2 text-sm sm:grid-cols-2 lg:grid-cols-4">
				<div class="rounded-box bg-base-200/50 p-3">
					<dt class="text-xs text-base-content/55">Identities evaluated</dt>
					<dd class="mt-1 font-semibold">
						{selected.repair_summary.total_identities.toLocaleString()}
					</dd>
				</div>
				<div class="rounded-box bg-base-200/50 p-3">
					<dt class="text-xs text-base-content/55">Still being checked</dt>
					<dd class="mt-1 font-semibold">
						{selected.repair_summary.remaining_identities.toLocaleString()}
					</dd>
				</div>
				<div class="rounded-box bg-base-200/50 p-3">
					<dt class="text-xs text-base-content/55">Tracks that stay playable</dt>
					<dd class="mt-1 font-semibold">
						{selected.repair_summary.playable_after_detach_track_count.toLocaleString()} of {selected.repair_summary.input_track_count.toLocaleString()}
					</dd>
				</div>
				<div class="rounded-box bg-base-200/50 p-3">
					<dt class="text-xs text-base-content/55">Evidence unavailable</dt>
					<dd class="mt-1 font-semibold">
						{(
							selected.repair_summary.provider_deferred_count +
							selected.repair_summary.failed_evidence_count
						).toLocaleString()}
					</dd>
				</div>
			</dl>
			<p class="mt-3 text-xs text-base-content/55">
				{Object.entries(selected.repair_summary.album_counts_by_root)
					.map(([rootId, count]) => `${rootLabels[rootId] ?? rootId}: ${count.toLocaleString()}`)
					.join(' · ')}
				{#if Object.keys(selected.repair_summary.album_counts_by_root).length}
					·
				{/if}Matcher {selected.repair_summary.target_matcher_version} · catalog revision {selected
					.repair_summary.catalog_snapshot_revision}
			</p>
			<div class="tabs tabs-box mt-3 overflow-x-auto" role="tablist" aria-label="Repair findings">
				{#each tabs as tab (tab.id)}<button
						type="button"
						role="tab"
						class:tab-active={activeTab === tab.id}
						class="tab whitespace-nowrap"
						aria-selected={activeTab === tab.id}
						onclick={() => (activeTab = tab.id)}
						>{tab.label}
						<span class="badge badge-sm ml-1">{tabCount(selected, tab.id)}</span></button
					>{/each}
			</div>
			{#if findingsQuery.isLoading}<div
					class="skeleton mt-3 h-24"
				></div>{:else if findingsQuery.isError}<div class="alert alert-error mt-3 text-sm">
					Could not load the repair findings.
				</div>{:else if findings.length === 0}<p
					class="mt-3 rounded-box bg-base-200/50 p-4 text-sm text-base-content/60"
				>
					No findings in this result.
				</p>{:else}<div
					class="mt-3 divide-y divide-base-content/10 rounded-box border border-base-content/10"
				>
					{#each findings as finding (finding.id)}<article
							class="grid gap-2 p-3 sm:grid-cols-[1fr_auto] sm:items-center"
						>
							<div>
								<strong>{finding.reason_code.replaceAll('_', ' ').toLowerCase()}</strong>
								<p class="mt-1 text-xs text-base-content/55">
									{finding.confidence} evidence · {finding.state}
								</p>
							</div>
							<a
								class="btn btn-ghost btn-xs"
								href={finding.review_id
									? `/library/review?review=${finding.review_id}`
									: `/album/${finding.local_album_id}`}
								>{finding.review_id ? 'Open evidence' : 'Open local album'}</a
							>
						</article>{/each}
				</div>{/if}
			{#if findingsQuery.hasNextPage}<button
					class="btn btn-ghost btn-sm mt-3"
					disabled={findingsQuery.isFetchingNextPage}
					onclick={() => void findingsQuery.fetchNextPage()}>Load more findings</button
				>{/if}
		</section>
	{/if}
</section>

<dialog
	bind:this={startDialog}
	class="modal"
	aria-labelledby="repair-start-title"
	onclose={() => (startOpen = false)}
>
	<div class="modal-box max-w-lg">
		<h2
			bind:this={startHeading}
			id="repair-start-title"
			tabindex="-1"
			class="flex items-center gap-2 text-lg font-bold"
		>
			<ShieldCheck class="h-5 w-5 text-primary" /> Check existing matches
		</h2>
		<p class="mt-3 text-sm text-base-content/70">
			Check current album identities against the safer matching rules. This scan does not change
			your library or files.
		</p>
		<fieldset class="mt-4 space-y-2">
			<legend class="text-sm font-semibold">Scope</legend>
			<label
				class="flex cursor-pointer items-center gap-2 rounded-box border border-base-content/10 p-3 text-sm"
			>
				<input
					type="radio"
					class="radio radio-sm"
					name="repair-scope"
					checked={scopeMode === 'all'}
					onchange={() => (scopeMode = 'all')}
				/>
				<span
					><strong>Whole library</strong><span class="block text-xs text-base-content/55"
						>Check every imported identity.</span
					></span
				>
			</label>
			<label
				class="flex cursor-pointer items-center gap-2 rounded-box border border-base-content/10 p-3 text-sm"
			>
				<input
					type="radio"
					class="radio radio-sm"
					name="repair-scope"
					checked={scopeMode === 'selected'}
					onchange={chooseSelectedRoots}
				/>
				<span
					><strong>Selected roots</strong><span class="block text-xs text-base-content/55"
						>Limit this check to chosen library roots.</span
					></span
				>
			</label>
			{#if scopeMode === 'selected'}
				<div class="ml-4 space-y-1 border-l border-base-content/15 pl-3">
					{#each roots as root (root.id)}
						<label class="flex items-center gap-2 py-1 text-sm" class:opacity-50={!root.available}>
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={selectedRootIds.includes(root.id)}
								disabled={!root.available}
								onchange={(event) => toggleRoot(root.id, event.currentTarget.checked)}
							/>
							{root.label}{#if !root.available}
								<span class="text-xs">Unavailable</span>{/if}
						</label>
					{/each}
				</div>
			{/if}
		</fieldset>
		<div class="mt-3 rounded-box bg-base-200 p-3 text-sm" aria-live="polite">
			{#if estimateQuery.isLoading}
				<span class="skeleton block h-5 w-48"></span>
			{:else if estimateQuery.isError}
				<span class="text-error">Could not estimate this check.</span>
			{:else if estimateQuery.data}
				<strong>{estimateQuery.data.identity_count.toLocaleString()} identities</strong> will be
				checked.
				<span class="mt-1 block text-xs text-base-content/60">
					{estimateQuery.data.queued_repair_count === 0
						? 'No other repair checks are waiting.'
						: `${estimateQuery.data.queued_repair_count.toLocaleString()} other repair checks are queued or active.`}
					This is a dry run and joins the repair work queue.
				</span>
			{/if}
		</div>
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => startDialog.close()}>Cancel</button>
			<button
				class="btn btn-primary"
				disabled={createRepair.isPending || estimateQuery.isLoading || !estimateQuery.data}
				onclick={() => void startCheck()}
			>
				{#if createRepair.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Start
				check
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close repair dialog">close</button>
	</form>
</dialog>

<dialog bind:this={applyDialog} class="modal" aria-labelledby="repair-apply-title">
	<div class="modal-box max-w-lg">
		<h2 bind:this={applyHeading} id="repair-apply-title" tabindex="-1" class="text-lg font-bold">
			Apply safe identity repairs?
		</h2>
		<p class="mt-3 text-sm text-base-content/70">
			External identities will be removed from these albums. Local files, album IDs, playlists, and
			playback will stay unchanged.
		</p>
		{#if selected?.repair_summary}
			<p class="mt-3 rounded-box bg-base-200 p-3 font-semibold">
				{selected.repair_summary.estimated_apply_changes.toLocaleString()} identities are eligible.
			</p>
		{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => applyDialog.close()}>Cancel</button>
			<button
				class="btn btn-warning"
				disabled={applyRepair.isPending}
				onclick={() => void applySelected()}
			>
				{#if applyRepair.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Apply
				safe repairs
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close apply dialog">close</button>
	</form>
</dialog>
