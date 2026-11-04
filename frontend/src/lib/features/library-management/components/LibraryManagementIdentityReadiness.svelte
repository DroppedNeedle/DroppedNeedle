<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { ArrowRight, BookOpenCheck, Database, ShieldCheck, Tags, Trash2 } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { createControlLibraryOperationMutation } from '$lib/features/library-management/queries/LibraryOperationMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import OperationJobStrip from './OperationJobStrip.svelte';
	import {
		createApplyLibraryIdentityPreparationMutation,
		createLibraryIdentityPreparationMutation,
		createDiscardLibraryIdentityPreparationMutation,
		createUndoLibraryAutomaticEditionMutation
	} from '$lib/features/library/queries/LibraryIdentityPreparationMutations.svelte';
	import {
		getLibraryIdentityPreparationEstimateQuery,
		getLibraryIdentityPreparationFindingsQuery,
		getLibraryIdentityPreparationsQuery
	} from '$lib/features/library/queries/LibraryIdentityPreparationQueries.svelte';
	import type {
		LibraryRootSettings,
		OperationResponse
	} from '$lib/features/library-management/queries/LibraryOperationsTypes';

	interface Props {
		roots: LibraryRootSettings[];
	}

	let { roots }: Props = $props();
	let startOpen = $state(false);
	let confirmOpen = $state(false);
	let scopeMode = $state<'all' | 'selected'>('all');
	let selectedRootIds = $state<string[]>([]);
	let confirmAction = $state<'apply' | 'discard'>('apply');
	let activeTab = $state<
		| 'ready'
		| 'mapping_ready'
		| 'exact_release_required'
		| 'exact_release_auto_accepted'
		| 'needs_review'
		| 'unverifiable'
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
	const createPreparation = createLibraryIdentityPreparationMutation(() => authStore.user?.id);
	const applyPreparation = createApplyLibraryIdentityPreparationMutation(() => authStore.user?.id);
	const discardPreparation = createDiscardLibraryIdentityPreparationMutation(
		() => authStore.user?.id
	);
	const undoEdition = createUndoLibraryAutomaticEditionMutation(() => authStore.user?.id);
	const pause = createControlLibraryOperationMutation('pause');
	const resume = createControlLibraryOperationMutation('resume');
	const stop = createControlLibraryOperationMutation('stop');

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
	const findingsMeta = $derived(findingsQuery.data?.pages[0] ?? null);
	const refreshRequired = $derived(findingsMeta?.refresh_required ?? false);
	const currentCounts = $derived(findingsMeta?.current_counts_by_finding ?? null);
	const suggestedCount = $derived(currentCounts?.exact_release_suggested ?? 0);
	const applySealSummary = $derived.by(() => {
		const mappings = currentCounts?.mapping_ready ?? 0;
		const editions = currentCounts?.exact_release_suggested ?? 0;
		const kinds = [
			mappings === 1 ? '1 exact track map' : mappings > 1 ? `${mappings} exact track maps` : null,
			editions === 1
				? '1 suggested edition'
				: editions > 1
					? `${editions} suggested editions`
					: null
		].filter((label): label is string => label !== null);
		const subject =
			kinds.length === 2
				? `${kinds[0]} and ${kinds[1]}`
				: kinds.length === 1
					? kinds[0]
					: 'No identities';
		return `${subject} will be sealed as durable catalog identity. Editions are chosen by stored evidence; ties break to the earliest Official edition, worldwide first. Override any edition afterwards from the album page.`;
	});
	const tabs = [
		{ id: 'mapping_ready', label: 'Mappings ready' },
		{ id: 'ready', label: 'Already ready' },
		{ id: 'exact_release_required', label: 'Choose edition' },
		{ id: 'exact_release_auto_accepted', label: 'Auto-accepted' },
		{ id: 'needs_review', label: 'Needs review' },
		{ id: 'unverifiable', label: 'Try again later' }
	] as const;

	function openStart(): void {
		scopeMode = 'all';
		selectedRootIds = [];
		startOpen = true;
	}

	async function undoAutoAccepted(albumId: string): Promise<void> {
		const finding = findings.find((item) => item.local_album_id === albumId);
		if (!finding?.automatic_undo) return;
		await undoEdition.mutateAsync({
			albumId,
			expectedAlbumRevision: finding.automatic_undo.expected_album_revision,
			expectedIdentityRevision: finding.automatic_undo.expected_identity_revision
		});
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
		startOpen = false;
	}

	function tabCount(item: OperationResponse, tab: (typeof tabs)[number]['id']): number {
		const counts = currentCounts ?? item.repair_summary?.counts_by_finding ?? {};
		return tab === 'unverifiable'
			? (counts.unverifiable ?? 0) + (counts.stale ?? 0)
			: tab === 'exact_release_required'
				? (counts.exact_release_required ?? 0) + (counts.exact_release_suggested ?? 0)
				: (counts[tab] ?? 0);
	}

	function openConfirmation(action: 'apply' | 'discard'): void {
		confirmAction = action;
		confirmOpen = true;
	}

	async function confirmReportAction(): Promise<void> {
		if (!report) return;
		const request = { jobId: report.id, expectedRevision: report.row_revision };
		if (confirmAction === 'apply') await applyPreparation.mutateAsync(request);
		else await discardPreparation.mutateAsync(request);
	}

	function findingTitle(reasonCode: string): string {
		return (
			{
				EXACT_RELEASE_MAPPING_SUPPORTED: 'Exact track map verified',
				EXACT_RELEASE_MAPPINGS_PRESENT: 'Exact track map already present',
				EXACT_EDITION_NOT_ACCEPTED: 'Choose the exact MusicBrainz edition',
				EXACT_EDITION_SUGGESTED: 'Exact edition suggested',
				EXACT_EDITION_AUTO_ACCEPTED: 'Edition accepted automatically',
				SELECTED_RELEASE_UNAVAILABLE: 'Selected edition is unavailable',
				SELECTED_RELEASE_CONFLICT: 'Selected edition conflicts with the release',
				CONFLICTING_TRACK_EVIDENCE: 'Track evidence conflicts',
				RELEASE_TYPE_REQUIRES_CONFIRMATION: 'Compilation or live edition needs confirmation',
				UNSAFE_RELEASE_TYPE: 'Compilation or live edition needs confirmation',
				PROVIDER_DEFERRED: 'MusicBrainz could not be reached',
				IDENTITY_CHANGED: 'Release changed during the check',
				STALE_SUBJECT: 'Release changed before Apply'
			}[reasonCode] ?? reasonCode.replaceAll('_', ' ').toLowerCase()
		);
	}
</script>

<section
	id="identity-readiness"
	tabindex="-1"
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
			<p class="mt-1 max-w-3xl text-sm text-muted">
				Give file organization the exact MusicBrainz edition and per-track map it needs. The check
				never changes tags, paths, or audio.
			</p>
		</div>
		<Button
			variant="outline"
			size="sm"
			icon={ShieldCheck}
			disabled={Boolean(active)}
			onclick={openStart}>Prepare identities...</Button
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
				<span class="text-xs text-muted">Ready now</span>
				<strong class="mt-1 block text-lg"
					>{wholeLibraryEstimate.data.ready_album_count.toLocaleString()}</strong
				>
			</div>
			<div class="bg-base-100/95 p-3 sm:px-4">
				<span class="text-xs text-muted">Need exact track maps</span>
				<strong class="mt-1 block text-lg"
					>{wholeLibraryEstimate.data.mapping_required_count.toLocaleString()}</strong
				>
			</div>
			<div class="bg-base-100/95 p-3 sm:px-4">
				<span class="text-xs text-muted">Need an exact edition</span>
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
				<strong>Checking release identities</strong>
				<p class="text-sm text-muted">
					{active.completed_count.toLocaleString()} of {active.expected_work_count.toLocaleString()}
					releases checked
				</p>
			</div>
			<OperationJobStrip
				size="xs"
				status={active.state}
				controlRequest={active.control_request}
				onpause={() =>
					pause
						.mutateAsync({ jobId: active.id, expectedRevision: active.row_revision })
						.catch(() => undefined)}
				onresume={() =>
					resume
						.mutateAsync({ jobId: active.id, expectedRevision: active.row_revision })
						.catch(() => undefined)}
				onstop={() =>
					stop
						.mutateAsync({ jobId: active.id, expectedRevision: active.row_revision })
						.catch(() => undefined)}
			/>
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
					<p class="mt-1 text-sm text-muted">
						{report.repair_summary.total_identities.toLocaleString()} releases checked · {report.repair_summary.mapping_candidate_count.toLocaleString()}
						exact track maps can be accepted
					</p>
				</div>
				<div class="flex flex-wrap gap-1">
					{#if report.state === 'ready' && !refreshRequired && (currentCounts?.mapping_ready ?? report.repair_summary.mapping_candidate_count) > 0}
						<Button
							variant="primary"
							size="sm"
							icon={Database}
							onclick={() => openConfirmation('apply')}>Accept mappings...</Button
						>
					{/if}
					{#if report.state === 'ready' && !refreshRequired && suggestedCount > 0}
						<Button variant="primary" size="sm" onclick={() => openConfirmation('apply')}
							>Accept editions ({suggestedCount})...</Button
						>
					{/if}
					{#if report.state === 'ready'}
						<Button
							variant="ghost"
							size="sm"
							icon={Trash2}
							onclick={() => openConfirmation('discard')}>Dismiss report</Button
						>
					{/if}
				</div>
			</div>

			{#if report.state === 'succeeded'}
				<div class="alert alert-success mt-3 text-sm">
					<Tags class="h-4 w-4" /> Catalog mappings are ready. Run a fresh organization preview to see
					what is now eligible.
				</div>
			{/if}

			{#if refreshRequired}
				<div class="alert alert-warning mt-3 text-sm" role="status">
					These checks used older rules. Run a fresh identity check.
				</div>
			{/if}

			<div class="tabs tabs-box mt-4 overflow-x-auto" role="group" aria-label="Identity findings">
				{#each tabs as tab (tab.id)}
					<button
						type="button"
						class="tab whitespace-nowrap"
						class:tab-active={activeTab === tab.id}
						aria-pressed={activeTab === tab.id}
						onclick={() => (activeTab = tab.id)}
						>{tab.label}<span class="badge badge-sm ml-1">{tabCount(report, tab.id)}</span></button
					>
				{/each}
			</div>
			<p class="mt-3 text-xs leading-5 text-muted">
				Only current findings are listed. Resolved and superseded ones stay in the audit history.
			</p>

			{#if findingsQuery.isLoading}
				<div class="skeleton mt-3 h-20"></div>
			{:else if findingsQuery.isError}
				<div class="alert alert-error mt-3 text-sm">Could not load these identity findings.</div>
			{:else if findings.length === 0}
				<p class="mt-3 rounded-box bg-base-200/50 p-4 text-sm text-muted">
					No releases in this category.
				</p>
			{:else}
				<div
					class="mt-3 max-h-96 divide-y divide-base-content/10 overflow-y-auto rounded-box border border-base-content/10 bg-base-100"
				>
					{#each findings as finding (finding.id)}
						<div class="flex items-center gap-3 p-3">
							<AlbumImage
								albumId={finding.local_album_id}
								available={finding.cover_available}
								alt={`Cover for ${finding.album_title}`}
								size="sm"
								requestSize={250}
								className="h-12 w-12 border border-base-content/10"
							/>
							<div class="min-w-0 flex-1">
								<div class="flex min-w-0 items-baseline gap-2">
									<strong class="truncate text-sm">{finding.album_title}</strong>
									{#if finding.album_year}
										<span class="shrink-0 text-xs text-muted">{finding.album_year}</span>
									{/if}
								</div>
								<p class="truncate text-xs text-muted">
									{finding.album_artist_name || 'Unknown release artist'}
								</p>
								{#if finding.suggested_edition}
									<div class="mt-1 flex min-w-0 items-center gap-2">
										<p class="truncate text-xs text-muted">
											Suggested: {finding.suggested_edition.title} · {finding.suggested_edition
												.date ?? 'date unknown'}{finding.suggested_edition.country
												? ` · ${finding.suggested_edition.country}`
												: ''}{finding.suggested_edition.status
												? ` · ${finding.suggested_edition.status}`
												: ''} · {finding.suggested_edition.track_count} tracks
										</p>
										{#if finding.suggested_edition.competing_count > 1}
											<span class="badge badge-sm badge-outline shrink-0"
												>1 of {finding.suggested_edition.competing_count} matching editions</span
											>
										{/if}
									</div>
								{/if}
								{#if finding.suggested_edition?.auto_gate && finding.suggested_edition.auto_gate !== 'AUTO_ACCEPT'}
									<p class="mt-1 text-xs text-warning">
										Held for review by the acceptance gate ({finding.suggested_edition.auto_gate
											.replaceAll('_', ' ')
											.toLowerCase()})
									</p>
								{/if}
								<p class="mt-1 text-xs text-muted">
									{finding.reason?.message ?? findingTitle(finding.reason_code)}
								</p>
								{#if finding.reason?.action}
									<p class="mt-0.5 text-xs text-muted">{finding.reason.action}</p>
								{/if}
								{#if finding.state === 'stale'}
									<p class="mt-0.5 text-xs text-warning">Changed after this report</p>
								{/if}
							</div>
							{#if !refreshRequired && activeTab === 'exact_release_auto_accepted'}
								<div class="flex shrink-0 items-center gap-2">
									<span class="badge badge-success badge-sm shrink-0">Auto-accepted</span>
									{#if finding.automatic_undo}
										<Button
											variant="danger-ghost"
											size="xs"
											disabled={undoEdition.isPending}
											onclick={() => void undoAutoAccepted(finding.local_album_id)}
										>
											Undo
										</Button>
									{/if}
								</div>
							{:else if !refreshRequired && (activeTab === 'exact_release_required' || activeTab === 'needs_review')}
								<Button
									variant="ghost"
									size="xs"
									icon={ArrowRight}
									iconPosition="end"
									href={withBasePath(
										`/album/${encodeURIComponent(finding.local_album_id)}?edition=choose`
									)}>Choose edition</Button
								>
							{:else if !refreshRequired}
								<Button
									variant="ghost"
									size="xs"
									icon={ArrowRight}
									iconPosition="end"
									href={withBasePath(`/album/${encodeURIComponent(finding.local_album_id)}`)}
									>Open release</Button
								>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
			{#if findingsQuery.hasNextPage}
				<Button
					variant="ghost"
					size="sm"
					class="mt-3"
					disabled={findingsQuery.isFetchingNextPage}
					onclick={() => void findingsQuery.fetchNextPage()}
					>{findingsQuery.isFetchingNextPage ? 'Loading...' : 'Load more releases'}</Button
				>
			{/if}
		</div>
	{/if}
</section>

<Modal bind:open={startOpen} title="Prepare identities" icon={ShieldCheck} size="lg" focusTitle>
	<p class="text-sm text-muted">
		This dry run checks exact MusicBrainz editions and track mappings. It reads the catalog and
		MusicBrainz; it does not write music files.
	</p>
	<fieldset class="mt-4 space-y-2">
		<legend class="text-sm font-semibold">Scope</legend>
		<label class="flex cursor-pointer gap-3 rounded-box border border-base-content/10 p-3 text-sm">
			<input
				type="radio"
				class="radio radio-sm"
				name="identity-preparation-scope"
				checked={scopeMode === 'all'}
				onchange={() => (scopeMode = 'all')}
			/>
			<span
				><strong>Whole library</strong><small class="block text-muted"
					>Every active release in every root.</small
				></span
			>
		</label>
		<label class="flex cursor-pointer gap-3 rounded-box border border-base-content/10 p-3 text-sm">
			<input
				type="radio"
				class="radio radio-sm"
				name="identity-preparation-scope"
				checked={scopeMode === 'selected'}
				onchange={chooseSelectedRoots}
			/>
			<span
				><strong>Selected roots</strong><small class="block text-muted"
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
						><strong>{root.label}</strong><small class="block truncate text-muted"
							>{root.path}</small
						></span
					>
				</label>
			{/each}
		</div>
	{/if}
	{#if startEstimate.data}
		<p class="mt-4 rounded-box bg-base-200/60 p-3 text-sm">
			<strong>{startEstimate.data.album_count.toLocaleString()} releases</strong> · {startEstimate.data.mapping_required_count.toLocaleString()}
			need track maps · {startEstimate.data.exact_release_required_count.toLocaleString()} need an exact
			edition
		</p>
	{/if}
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (startOpen = false)}>Cancel</Button>
		<Button
			variant="primary"
			loading={createPreparation.isPending}
			disabled={startEstimate.isLoading ||
				(scopeMode === 'selected' && selectedRootIds.length === 0)}
			onclick={() => void startPreparation()}
		>
			Start read-only check
		</Button>
	{/snippet}
</Modal>

<ConfirmDialog
	bind:open={confirmOpen}
	title={confirmAction === 'apply' ? 'Accept exact-release mappings?' : 'Dismiss this report?'}
	confirmLabel={confirmAction === 'apply' ? 'Accept identities' : 'Dismiss report'}
	tone={confirmAction === 'apply' ? 'primary' : 'danger'}
	focusTitle
	pending={applyPreparation.isPending || discardPreparation.isPending}
	onconfirm={confirmReportAction}
>
	{#if confirmAction === 'apply'}
		<p class="text-sm text-muted">
			This writes only verified MusicBrainz identities to DroppedNeedle's catalog. It does not
			change tags, paths, or audio. Releases may become eligible for a future organization preview.
		</p>
		<p class="mt-2 text-sm text-muted">{applySealSummary}</p>
	{:else}
		<p class="text-sm text-muted">
			This removes the report from the active workspace. Its audit record remains, and no music file
			or catalog identity is changed.
		</p>
	{/if}
</ConfirmDialog>
