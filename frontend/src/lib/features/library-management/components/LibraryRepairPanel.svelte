<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { ShieldCheck, Wrench } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import OperationJobStrip from './OperationJobStrip.svelte';
	import {
		getLibraryRepairEstimateQuery,
		getLibraryRepairFindingsQuery,
		getLibraryRepairsQuery
	} from '$lib/features/library-management/queries/LibraryRepairQueries.svelte';
	import { getLibraryPolicyTreeQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
	import { createControlLibraryOperationMutation } from '$lib/features/library-management/queries/LibraryOperationMutations.svelte';
	import {
		createApplyLibraryRepairMutation,
		createLibraryRepairMutation
	} from '$lib/features/library-management/queries/LibraryRepairMutations.svelte';
	import type { OperationResponse } from '$lib/features/library-management/queries/LibraryOperationsTypes';
	import { withBasePath } from '$lib/utils/basePath';

	const DEFAULT_VISIBLE_REPAIRS = 3;
	const repairsQuery = getLibraryRepairsQuery();
	const policyTreeQuery = getLibraryPolicyTreeQuery();
	const createRepair = createLibraryRepairMutation();
	const applyRepair = createApplyLibraryRepairMutation();
	let applyOpen = $state(false);
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
	const pause = createControlLibraryOperationMutation('pause');
	const resume = createControlLibraryOperationMutation('resume');
	const stop = createControlLibraryOperationMutation('stop');
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
		applyOpen = true;
	}

	async function startCheck(): Promise<void> {
		try {
			await createRepair.mutateAsync(estimateRootIds);
		} catch {
			return;
		}
		startOpen = false;
	}

	async function applySelected(): Promise<void> {
		if (!selected) return;
		await applyRepair.mutateAsync({
			jobId: selected.id,
			expectedRevision: selected.row_revision
		});
	}

	function visibleState(repair: OperationResponse): string {
		if (repair.control_request === 'stop') return 'stopping';
		if (repair.control_request === 'pause') return 'pausing';
		return repair.state;
	}
</script>

<Card
	as="section"
	id="library-repair"
	tabindex={-1}
	variant="bordered"
	padding="none"
	class="lm-bordered"
	aria-label="Repair"
>
	<div class="flex flex-wrap items-center gap-4 p-4 sm:px-5">
		<Wrench class="size-7 flex-none text-warning" aria-hidden="true" />
		<div class="min-w-0 flex-1">
			<h3 class="lm-cap-title">Repair</h3>
			<p class="text-sm text-muted">Re-check albums matched by older identification rules.</p>
		</div>
		<Button variant="outline" size="sm" onclick={openStart}>Check existing matches</Button>
	</div>

	{#if repairsQuery.isLoading}
		<div class="border-t border-base-content/10 p-4"><div class="skeleton h-12"></div></div>
	{:else if repairs.length > 0}
		<div class="divide-y divide-base-content/10 border-t border-base-content/10">
			{#each visibleRepairs as repair (repair.id)}
				<div class="grid gap-3 p-4 sm:grid-cols-[1fr_auto] sm:items-center">
					<div>
						<div class="flex flex-wrap items-center gap-2">
							<strong>Identity recheck</strong>
							<span class="badge badge-ghost badge-sm">{visibleState(repair)}</span>
						</div>
						<p class="mt-1 text-sm text-muted">
							{repair.completed_count.toLocaleString()} of {repair.expected_work_count.toLocaleString()}
							checked · {repair.failed_count.toLocaleString()} failed · {repair.skipped_count.toLocaleString()}
							stale
						</p>
						{#if repair.repair_summary}
							<p class="mt-1 text-xs text-muted">
								{repair.repair_summary.estimated_apply_changes.toLocaleString()} safe to detach · {repair.repair_summary.remaining_identities.toLocaleString()}
								identities remain
							</p>
						{/if}
					</div>
					<OperationJobStrip
						class="justify-end"
						size="xs"
						status={repair.state}
						controlRequest={repair.control_request}
						onpause={() =>
							pause
								.mutateAsync({ jobId: repair.id, expectedRevision: repair.row_revision })
								.catch(() => undefined)}
						onresume={() =>
							resume
								.mutateAsync({ jobId: repair.id, expectedRevision: repair.row_revision })
								.catch(() => undefined)}
						onstop={() =>
							stop
								.mutateAsync({ jobId: repair.id, expectedRevision: repair.row_revision })
								.catch(() => undefined)}
					>
						{#if repair.repair_summary}<Button
								variant="outline"
								size="xs"
								onclick={() => openReport(repair)}>View report</Button
							>{/if}
						{#if repair.state === 'ready' && repair.repair_summary?.estimated_apply_changes}<Button
								variant="warning"
								size="xs"
								onclick={() => openApply(repair)}>Apply safe repairs...</Button
							>{/if}
					</OperationJobStrip>
					{#if repair.state === 'succeeded' && repair.repair_summary?.estimated_apply_changes}
						<a
							class="text-sm link link-primary sm:col-span-2 sm:justify-self-end"
							href={withBasePath('/library/review?view=unmatched')}>Review detached albums</a
						>
					{/if}
				</div>
			{/each}
			{#if olderRepairCount > 0}
				<div class="flex justify-center p-2">
					<Button variant="ghost" size="xs" onclick={() => (showOlder = !showOlder)}>
						{showOlder
							? `Show latest ${DEFAULT_VISIBLE_REPAIRS}`
							: `Show ${olderRepairCount} older ${olderRepairCount === 1 ? 'check' : 'checks'}`}
					</Button>
				</div>
			{/if}
		</div>
	{/if}

	{#if selected?.repair_summary}
		<section class="border-t border-base-content/10 p-4" aria-labelledby="repair-report-title">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<h4 id="repair-report-title" class="font-semibold">Recheck report</h4>
				<Button variant="ghost" size="xs" onclick={() => (selected = null)}>Close report</Button>
			</div>
			<dl class="mt-3 grid gap-2 text-sm sm:grid-cols-2 lg:grid-cols-4">
				<div class="rounded-box bg-base-100/50 p-3">
					<dt class="text-xs text-muted">Identities evaluated</dt>
					<dd class="mt-1 font-semibold">
						{selected.repair_summary.total_identities.toLocaleString()}
					</dd>
				</div>
				<div class="rounded-box bg-base-100/50 p-3">
					<dt class="text-xs text-muted">Still being checked</dt>
					<dd class="mt-1 font-semibold">
						{selected.repair_summary.remaining_identities.toLocaleString()}
					</dd>
				</div>
				<div class="rounded-box bg-base-100/50 p-3">
					<dt class="text-xs text-muted">Tracks that stay playable</dt>
					<dd class="mt-1 font-semibold">
						{selected.repair_summary.playable_after_detach_track_count.toLocaleString()} of {selected.repair_summary.input_track_count.toLocaleString()}
					</dd>
				</div>
				<div class="rounded-box bg-base-100/50 p-3">
					<dt class="text-xs text-muted">Evidence unavailable</dt>
					<dd class="mt-1 font-semibold">
						{(
							selected.repair_summary.provider_deferred_count +
							selected.repair_summary.failed_evidence_count
						).toLocaleString()}
					</dd>
				</div>
			</dl>
			<p class="mt-3 text-xs text-muted">
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
					class="mt-3 rounded-box bg-base-100/50 p-4 text-sm text-muted"
				>
					No findings in this result.
				</p>{:else}<div
					class="mt-3 divide-y divide-base-content/10 rounded-box border border-base-content/10"
				>
					{#each findings as finding (finding.id)}<article
							class="grid gap-2 p-3 sm:grid-cols-[1fr_auto] sm:items-center"
						>
							<div class="min-w-0">
								<strong class="block truncate"
									>{finding.album_title}{finding.album_artist_name
										? ` · ${finding.album_artist_name}`
										: ''}</strong
								>
								<p class="mt-1 text-sm">
									{finding.reason?.message ??
										finding.reason_code.replaceAll('_', ' ').toLowerCase()}
								</p>
								{#if finding.reason?.action}
									<p class="mt-0.5 text-xs text-muted">{finding.reason.action}</p>
								{/if}
								<p class="mt-1 text-xs text-muted">
									{finding.state === 'applied'
										? 'Applied'
										: finding.state === 'stale'
											? 'Changed after this report'
											: finding.apply_eligible
												? 'Safe to apply'
												: 'Report only'}
								</p>
							</div>
							<Button
								variant="ghost"
								size="xs"
								href={finding.review_id
									? withBasePath(`/library/review?review=${finding.review_id}`)
									: withBasePath(`/album/${finding.local_album_id}`)}
								>{finding.review_id ? 'Open evidence' : 'Open local album'}</Button
							>
						</article>{/each}
				</div>{/if}
			{#if findingsQuery.hasNextPage}<Button
					variant="ghost"
					size="sm"
					class="mt-3"
					disabled={findingsQuery.isFetchingNextPage}
					onclick={() => void findingsQuery.fetchNextPage()}>Load more findings</Button
				>{/if}
		</section>
	{/if}
</Card>

<Modal bind:open={startOpen} title="Check existing matches" icon={ShieldCheck} focusTitle>
	<p class="text-sm text-muted">
		Check current album identities against the latest matching rules. This scan does not change your
		library or files.
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
				><strong>Whole library</strong><span class="block text-xs text-muted"
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
				><strong>Selected roots</strong><span class="block text-xs text-muted"
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
			<span class="mt-1 block text-xs text-muted">
				{estimateQuery.data.queued_repair_count === 0
					? 'No other repair checks are waiting.'
					: `${estimateQuery.data.queued_repair_count.toLocaleString()} other repair checks are queued or active.`}
				This is a dry run and joins the repair work queue.
			</span>
		{/if}
	</div>
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (startOpen = false)}>Cancel</Button>
		<Button
			variant="primary"
			loading={createRepair.isPending}
			disabled={estimateQuery.isLoading || !estimateQuery.data}
			onclick={() => void startCheck()}>Start check</Button
		>
	{/snippet}
</Modal>

<ConfirmDialog
	bind:open={applyOpen}
	title="Apply safe identity repairs?"
	message="External identities will be removed from these albums. Local files, album IDs, playlists, and playback will stay unchanged."
	confirmLabel="Apply safe repairs"
	tone="warning"
	focusTitle
	pending={applyRepair.isPending}
	onconfirm={applySelected}
>
	{#if selected?.repair_summary}
		<p class="mt-3 rounded-box bg-base-200 p-3 font-semibold">
			{selected.repair_summary.estimated_apply_changes.toLocaleString()} identities are eligible.
		</p>
	{/if}
</ConfirmDialog>
