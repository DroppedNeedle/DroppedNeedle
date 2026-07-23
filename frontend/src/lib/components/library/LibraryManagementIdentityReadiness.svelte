<script lang="ts">
	import {
		ArrowRight,
		BookOpenCheck,
		CirclePause,
		CirclePlay,
		Database,
		OctagonX,
		ShieldCheck,
		Tags,
		Trash2
	} from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { controlLibraryOperation } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import {
		applyLibraryIdentityPreparation,
		createLibraryIdentityPreparation,
		discardLibraryIdentityPreparation
	} from '$lib/queries/library/LibraryIdentityPreparationMutations.svelte';
	import {
		getLibraryIdentityPreparationEstimateQuery,
		getLibraryIdentityPreparationFindingsQuery,
		getLibraryIdentityPreparationsQuery
	} from '$lib/queries/library/LibraryIdentityPreparationQueries.svelte';
	import type {
		LibraryRootSettings,
		OperationResponse
	} from '$lib/queries/library/LibraryOperationsTypes';

	interface Props {
		roots: LibraryRootSettings[];
	}

	let { roots }: Props = $props();
	let startDialog: HTMLDialogElement;
	let confirmDialog: HTMLDialogElement;
	let startHeading: HTMLHeadingElement;
	let confirmHeading: HTMLHeadingElement;
	let startOpen = $state(false);
	let scopeMode = $state<'all' | 'selected'>('all');
	let selectedRootIds = $state<string[]>([]);
	let confirmAction = $state<'apply' | 'discard'>('apply');
	let activeTab = $state<
		'ready' | 'mapping_ready' | 'exact_release_required' | 'needs_review' | 'unverifiable'
	>('mapping_ready');

	const preparationsQuery = getLibraryIdentityPreparationsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const wholeLibraryEstimate = getLibraryIdentityPreparationEstimateQuery(
		() => authStore.user?.id,
		() => [],
		() => authStore.isAdmin
	);
	const estimateRootIds = $derived(scopeMode === 'all' ? [] : selectedRootIds);
	const startEstimate = getLibraryIdentityPreparationEstimateQuery(
		() => authStore.user?.id,
		() => estimateRootIds,
		() => startOpen && (scopeMode === 'all' || selectedRootIds.length > 0)
	);
	const createPreparation = createLibraryIdentityPreparation(() => authStore.user?.id);
	const applyPreparation = applyLibraryIdentityPreparation(() => authStore.user?.id);
	const discardPreparation = discardLibraryIdentityPreparation(() => authStore.user?.id);
	const pause = controlLibraryOperation('pause');
	const resume = controlLibraryOperation('resume');
	const stop = controlLibraryOperation('stop');

	const preparations = $derived(preparationsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const active = $derived(
		preparations.find((item) => ['queued', 'running', 'paused'].includes(item.state)) ?? null
	);
	const latest = $derived(preparations[0] ?? null);
	const report = $derived(
		latest?.repair_summary && latest.terminal_code !== 'IDENTITY_PREPARATION_DISCARDED'
			? latest
			: null
	);
	const findingsQuery = getLibraryIdentityPreparationFindingsQuery(
		() => authStore.user?.id,
		() => report?.id ?? null,
		() => activeTab
	);
	const findings = $derived(findingsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const tabs = [
		{ id: 'mapping_ready', label: 'Mappings ready' },
		{ id: 'ready', label: 'Already ready' },
		{ id: 'exact_release_required', label: 'Choose edition' },
		{ id: 'needs_review', label: 'Needs review' },
		{ id: 'unverifiable', label: 'Try again later' }
	] as const;

	function openStart(): void {
		scopeMode = 'all';
		selectedRootIds = [];
		startOpen = true;
		startDialog.showModal();
		startHeading.focus();
	}

	function chooseSelectedRoots(): void {
		scopeMode = 'selected';
		if (selectedRootIds.length === 0) {
			selectedRootIds = roots.map((root) => root.id);
		}
	}

	function toggleRoot(rootId: string, checked: boolean): void {
		selectedRootIds = checked
			? [...selectedRootIds, rootId]
			: selectedRootIds.filter((id) => id !== rootId);
	}

	async function startPreparation(): Promise<void> {
		try {
			await createPreparation.mutateAsync(estimateRootIds);
		} catch {
			return;
		}
		startDialog.close();
	}

	function tabCount(item: OperationResponse, tab: (typeof tabs)[number]['id']): number {
		const counts = item.repair_summary?.counts_by_finding ?? {};
		return tab === 'unverifiable'
			? (counts.unverifiable ?? 0) + (counts.stale ?? 0)
			: (counts[tab] ?? 0);
	}

	function openConfirmation(action: 'apply' | 'discard'): void {
		confirmAction = action;
		confirmDialog.showModal();
		confirmHeading.focus();
	}

	async function confirmReportAction(): Promise<void> {
		if (!report) return;
		try {
			if (confirmAction === 'apply') {
				await applyPreparation.mutateAsync({
					jobId: report.id,
					expectedRevision: report.row_revision
				});
			} else {
				await discardPreparation.mutateAsync({
					jobId: report.id,
					expectedRevision: report.row_revision
				});
			}
		} catch {
			return;
		}
		confirmDialog.close();
	}

	function findingTitle(reasonCode: string): string {
		return (
			{
				EXACT_RELEASE_MAPPING_SUPPORTED: 'Exact track map verified',
				EXACT_RELEASE_MAPPINGS_PRESENT: 'Exact track map already present',
				EXACT_EDITION_NOT_ACCEPTED: 'Choose the exact MusicBrainz edition',
				SELECTED_RELEASE_UNAVAILABLE: 'Selected edition is unavailable',
				SELECTED_RELEASE_CONFLICT: 'Selected edition conflicts with the album',
				CONFLICTING_TRACK_EVIDENCE: 'Track evidence conflicts',
				PROVIDER_DEFERRED: 'MusicBrainz could not be reached',
				IDENTITY_CHANGED: 'Album changed during the check',
				STALE_SUBJECT: 'Album changed before Apply'
			}[reasonCode] ?? reasonCode.replaceAll('_', ' ').toLowerCase()
		);
	}
</script>

<section
	id="identity-readiness"
	class="rounded-box border border-primary/20 bg-primary/[0.035]"
	aria-labelledby="identity-readiness-title"
>
	<div class="flex flex-wrap items-start gap-3 p-4 sm:p-5">
		<div
			class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"
		>
			<BookOpenCheck class="h-5 w-5" aria-hidden="true" />
		</div>
		<div class="min-w-0 flex-1">
			<p class="management-step">Safe prerequisite</p>
			<h3 id="identity-readiness-title" class="font-display text-lg font-semibold">
				Identity readiness
			</h3>
			<p class="mt-1 max-w-3xl text-sm text-base-content/60">
				Give Library Management the exact MusicBrainz edition and per-track map it needs. The check
				never changes tags, paths, or audio.
			</p>
		</div>
		<button class="btn btn-outline btn-sm" disabled={Boolean(active)} onclick={openStart}
			><ShieldCheck class="h-4 w-4" /> Prepare identities...</button
		>
	</div>

	{#if wholeLibraryEstimate.isLoading}
		<div class="grid gap-2 border-t border-primary/10 p-4 sm:grid-cols-3">
			{#each Array(3) as _, index (index)}<div class="skeleton h-16"></div>{/each}
		</div>
	{:else if wholeLibraryEstimate.isError}
		<div class="border-t border-primary/10 p-4 text-sm text-error">
			Identity readiness could not be counted right now.
		</div>
	{:else if wholeLibraryEstimate.data}
		<div class="grid gap-px border-t border-primary/10 bg-primary/10 sm:grid-cols-3">
			<div class="bg-base-100/95 p-3 sm:px-4">
				<span class="text-xs text-base-content/50">Ready now</span>
				<strong class="mt-1 block text-lg"
					>{wholeLibraryEstimate.data.ready_album_count.toLocaleString()}</strong
				>
			</div>
			<div class="bg-base-100/95 p-3 sm:px-4">
				<span class="text-xs text-base-content/50">Need exact track maps</span>
				<strong class="mt-1 block text-lg"
					>{wholeLibraryEstimate.data.mapping_required_count.toLocaleString()}</strong
				>
			</div>
			<div class="bg-base-100/95 p-3 sm:px-4">
				<span class="text-xs text-base-content/50">Need an exact edition</span>
				<strong class="mt-1 block text-lg"
					>{wholeLibraryEstimate.data.exact_release_required_count.toLocaleString()}</strong
				>
			</div>
		</div>
	{/if}

	{#if active}
		<div class="flex flex-wrap items-center gap-3 border-t border-primary/10 p-4">
			<span class="management-live-dot" aria-hidden="true"></span>
			<div class="min-w-0 flex-1">
				<strong>Checking album identities</strong>
				<p class="text-sm text-base-content/55">
					{active.completed_count.toLocaleString()} of {active.expected_work_count.toLocaleString()}
					albums checked
				</p>
			</div>
			<div class="flex flex-wrap gap-1">
				{#if active.state === 'running'}
					<button
						class="btn btn-ghost btn-xs"
						onclick={() =>
							void pause
								.mutateAsync({ jobId: active.id, expectedRevision: active.row_revision })
								.catch(() => undefined)}><CirclePause class="h-3.5 w-3.5" /> Pause</button
					>
				{:else if active.state === 'paused'}
					<button
						class="btn btn-ghost btn-xs"
						onclick={() =>
							void resume
								.mutateAsync({ jobId: active.id, expectedRevision: active.row_revision })
								.catch(() => undefined)}><CirclePlay class="h-3.5 w-3.5" /> Resume</button
					>
				{/if}
				<button
					class="btn btn-ghost btn-xs text-error"
					disabled={active.control_request !== 'none'}
					onclick={() =>
						void stop
							.mutateAsync({ jobId: active.id, expectedRevision: active.row_revision })
							.catch(() => undefined)}><OctagonX class="h-3.5 w-3.5" /> Stop</button
				>
			</div>
		</div>
	{/if}

	{#if report?.repair_summary}
		<div class="border-t border-primary/10 p-4 sm:p-5">
			<div class="flex flex-wrap items-start justify-between gap-3">
				<div>
					<p class="management-step">Latest identity report</p>
					<h4 class="font-semibold">
						{report.state === 'succeeded' ? 'Mappings accepted' : 'Ready for review'}
					</h4>
					<p class="mt-1 text-sm text-base-content/55">
						{report.repair_summary.total_identities.toLocaleString()} albums checked · {report.repair_summary.mapping_candidate_count.toLocaleString()}
						exact track maps can be accepted
					</p>
				</div>
				<div class="flex flex-wrap gap-1">
					{#if report.state === 'ready' && report.repair_summary.mapping_candidate_count > 0}
						<button class="btn btn-primary btn-sm" onclick={() => openConfirmation('apply')}
							><Database class="h-4 w-4" /> Accept mappings...</button
						>
					{/if}
					{#if report.state === 'ready'}
						<button class="btn btn-ghost btn-sm" onclick={() => openConfirmation('discard')}
							><Trash2 class="h-4 w-4" /> Dismiss report</button
						>
					{/if}
				</div>
			</div>

			{#if report.state === 'succeeded'}
				<div class="alert alert-success mt-3 text-sm">
					<Tags class="h-4 w-4" /> Catalog mappings are ready. Run a fresh Library Management preview
					to see what is now eligible.
				</div>
			{/if}

			<div class="tabs tabs-box mt-4 overflow-x-auto" role="tablist" aria-label="Identity findings">
				{#each tabs as tab (tab.id)}
					<button
						type="button"
						role="tab"
						class="tab whitespace-nowrap"
						class:tab-active={activeTab === tab.id}
						aria-selected={activeTab === tab.id}
						onclick={() => (activeTab = tab.id)}
						>{tab.label}<span class="badge badge-sm ml-1">{tabCount(report, tab.id)}</span></button
					>
				{/each}
			</div>

			{#if findingsQuery.isLoading}
				<div class="skeleton mt-3 h-20"></div>
			{:else if findingsQuery.isError}
				<div class="alert alert-error mt-3 text-sm">Could not load these identity findings.</div>
			{:else if findings.length === 0}
				<p class="mt-3 rounded-box bg-base-200/50 p-4 text-sm text-base-content/55">
					No albums in this category.
				</p>
			{:else}
				<div
					class="mt-3 max-h-72 divide-y divide-base-content/10 overflow-y-auto rounded-box border border-base-content/10 bg-base-100"
				>
					{#each findings as finding (finding.id)}
						<div class="flex items-center gap-3 p-3">
							<div class="min-w-0 flex-1">
								<strong class="text-sm">{findingTitle(finding.reason_code)}</strong>
								<p class="mt-0.5 text-xs text-base-content/50">
									{finding.state === 'stale' ? 'Changed after this report' : finding.confidence}
								</p>
							</div>
							<a
								class="btn btn-ghost btn-xs"
								href={`/album/${encodeURIComponent(finding.local_album_id)}`}
								>{activeTab === 'exact_release_required'
									? 'Choose edition'
									: 'Open album'}<ArrowRight class="h-3.5 w-3.5" /></a
							>
						</div>
					{/each}
				</div>
			{/if}
			{#if findingsQuery.hasNextPage}
				<button
					class="btn btn-ghost btn-sm mt-3"
					disabled={findingsQuery.isFetchingNextPage}
					onclick={() => void findingsQuery.fetchNextPage()}
					>{findingsQuery.isFetchingNextPage ? 'Loading...' : 'Load more albums'}</button
				>
			{/if}
		</div>
	{/if}
</section>

<dialog bind:this={startDialog} class="modal" onclose={() => (startOpen = false)}>
	<div class="modal-box max-w-xl">
		<h2 bind:this={startHeading} tabindex="-1" class="flex items-center gap-2 text-lg font-bold">
			<ShieldCheck class="h-5 w-5 text-primary" /> Prepare identities
		</h2>
		<p class="mt-3 text-sm text-base-content/65">
			This dry run checks exact MusicBrainz editions and track mappings. It reads the catalog and
			MusicBrainz; it does not write music files.
		</p>
		<fieldset class="mt-4 space-y-2">
			<legend class="text-sm font-semibold">Scope</legend>
			<label
				class="flex cursor-pointer gap-3 rounded-box border border-base-content/10 p-3 text-sm"
			>
				<input
					type="radio"
					class="radio radio-sm"
					name="identity-preparation-scope"
					checked={scopeMode === 'all'}
					onchange={() => (scopeMode = 'all')}
				/>
				<span
					><strong>Whole library</strong><small class="block text-base-content/55"
						>Every active album in every root.</small
					></span
				>
			</label>
			<label
				class="flex cursor-pointer gap-3 rounded-box border border-base-content/10 p-3 text-sm"
			>
				<input
					type="radio"
					class="radio radio-sm"
					name="identity-preparation-scope"
					checked={scopeMode === 'selected'}
					onchange={chooseSelectedRoots}
				/>
				<span
					><strong>Selected roots</strong><small class="block text-base-content/55"
						>Limit the check to specific library roots.</small
					></span
				>
			</label>
		</fieldset>
		{#if scopeMode === 'selected'}
			<div class="mt-3 max-h-48 space-y-1 overflow-y-auto rounded-box bg-base-200/50 p-2">
				{#each roots as root (root.id)}
					<label class="flex items-center gap-2 rounded-lg px-2 py-2 text-sm">
						<input
							type="checkbox"
							class="checkbox checkbox-sm"
							checked={selectedRootIds.includes(root.id)}
							onchange={(event) => toggleRoot(root.id, event.currentTarget.checked)}
						/>
						<span class="min-w-0"
							><strong>{root.label}</strong><small class="block truncate text-base-content/50"
								>{root.path}</small
							></span
						>
					</label>
				{/each}
			</div>
		{/if}
		{#if startEstimate.data}
			<p class="mt-4 rounded-box bg-base-200/60 p-3 text-sm">
				<strong>{startEstimate.data.album_count.toLocaleString()} albums</strong> · {startEstimate.data.mapping_required_count.toLocaleString()}
				need track maps · {startEstimate.data.exact_release_required_count.toLocaleString()} need an exact
				edition
			</p>
		{/if}
		<div class="modal-action">
			<form method="dialog"><button class="btn btn-ghost">Cancel</button></form>
			<button
				class="btn btn-primary"
				disabled={createPreparation.isPending ||
					startEstimate.isLoading ||
					(scopeMode === 'selected' && selectedRootIds.length === 0)}
				onclick={() => void startPreparation()}
			>
				{#if createPreparation.isPending}<span class="loading loading-spinner loading-sm"
					></span>{/if}
				Start read-only check
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop"><button>Close</button></form>
</dialog>

<dialog bind:this={confirmDialog} class="modal">
	<div class="modal-box max-w-lg">
		<h2 bind:this={confirmHeading} tabindex="-1" class="text-lg font-bold">
			{confirmAction === 'apply' ? 'Accept exact-release mappings?' : 'Dismiss this report?'}
		</h2>
		{#if confirmAction === 'apply'}
			<p class="mt-3 text-sm text-base-content/65">
				This writes only verified MusicBrainz identities to DroppedNeedle's catalog. It does not
				change tags, paths, or audio. Albums may become eligible for a future Library Management
				preview.
			</p>
		{:else}
			<p class="mt-3 text-sm text-base-content/65">
				This removes the report from the active workspace. Its audit record remains, and no music
				file or catalog identity is changed.
			</p>
		{/if}
		<div class="modal-action">
			<form method="dialog"><button class="btn btn-ghost">Cancel</button></form>
			<button
				class:btn-primary={confirmAction === 'apply'}
				class:btn-error={confirmAction === 'discard'}
				class="btn"
				disabled={applyPreparation.isPending || discardPreparation.isPending}
				onclick={() => void confirmReportAction()}
			>
				{#if applyPreparation.isPending || discardPreparation.isPending}<span
						class="loading loading-spinner loading-sm"
					></span>{/if}
				{confirmAction === 'apply' ? 'Accept catalog mappings' : 'Dismiss report'}
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop"><button>Close</button></form>
</dialog>
