<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import {
		ArrowRight,
		BookOpenCheck,
		Clock3,
		FolderCog,
		HardDrive,
		Layers,
		ShieldAlert,
		Sparkles,
		Tags
	} from 'lucide-svelte';

	import BackButton from '$lib/components/BackButton.svelte';
	import LibraryManagementDiscardPreview from './LibraryManagementDiscardPreview.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { LIBRARY_MANAGEMENT_ENDPOINTS } from '$lib/queries/library-management/endpoints';
	import { createLibraryManagementEvents } from '$lib/queries/library-management/LibraryManagementEvents';
	import {
		getLibraryManagementPlanItemsQuery,
		getLibraryManagementPreviewQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import type {
		LibraryManagementPlanItem,
		ManagementEligibility
	} from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		firstDeferredSource,
		formatManagementBytes,
		formatManagementDate,
		managementApplyCopy,
		managementAudioFormat,
		managementAlbumArtworkVersion,
		managementArtworkPreviewHash,
		managementDesiredField,
		managementPlanAlbum,
		managementPlanAlbumArtist,
		managementPlanArtist,
		managementPlanChanges,
		managementPlanIsExceptional,
		managementPlanTitle,
		managementPlanTrackLabel,
		managementPreviewHeading,
		managementProviderStatus,
		managementQuantity,
		managementRootLabel,
		managementStaleBannerDetail,
		qualifiedManagementReasonLabel,
		titleManagementValue
	} from './LibraryManagementDisplay';
	import LibraryManagementApplyDialog from './LibraryManagementApplyDialog.svelte';
	import LibraryManagementAuditDossiers from './LibraryManagementAuditDossiers.svelte';
	import LibraryManagementCollisionDialog from './LibraryManagementCollisionDialog.svelte';
	import LibraryManagementPlanFilterBar from './LibraryManagementPlanFilterBar.svelte';
	import LibraryManagementPlanInspector from './LibraryManagementPlanInspector.svelte';
	import {
		groupManagementAuditEntries,
		type ManagementAuditEntry,
		type ManagementAuditTone
	} from './LibraryManagementAuditTypes';
	import { PreviewFilters } from './previewFilters.svelte';
	import { createPreviewToken } from './previewToken.svelte';

	interface Props {
		jobId: string;
	}

	let { jobId }: Props = $props();
	let applyDialog = $state<ReturnType<typeof LibraryManagementApplyDialog> | null>(null);
	let collisionDialog = $state<ReturnType<typeof LibraryManagementCollisionDialog> | null>(null);
	let stickyFooterElement: HTMLElement | null = $state(null);
	let stickyFooterHeight = $state(0);
	const filters = new PreviewFilters();

	const previewQuery = getLibraryManagementPreviewQuery(
		() => authStore.user?.id,
		() => jobId
	);
	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const policyQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const itemsQuery = getLibraryManagementPlanItemsQuery(
		() => authStore.user?.id,
		() => jobId,
		() => filters.params(50)
	);

	const preview = $derived(previewQuery.data ?? null);
	const items = $derived(itemsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const roots = $derived(policyQuery.data?.library_roots ?? []);
	const auditDossiers = $derived(
		groupManagementAuditEntries(items.map((item) => planAuditEntry(item)))
	);
	const applyCount = $derived(
		(preview?.summary.eligible_count ?? 0) + (preview?.summary.warning_count ?? 0)
	);
	const activationPreview = $derived(
		Boolean(preview && preview.proposed_settings_revision !== null)
	);
	const token = createPreviewToken(
		() => jobId,
		() => previewQuery.data ?? null,
		() => activationPreview
	);
	const zeroAppliable = $derived(
		Boolean(
			preview &&
			preview.state === 'ready' &&
			!preview.stale &&
			!preview.expired &&
			!activationPreview &&
			applyCount === 0
		)
	);
	const topBlockers = $derived(
		Object.entries(preview?.summary.reasons ?? {})
			.sort((left, right) => right[1] - left[1])
			.slice(0, 3)
	);
	const deferredSources = $derived(
		Object.entries(preview?.summary.deferred_sources ?? {})
			.sort((left, right) => right[1] - left[1])
			.slice(0, 3)
	);
	const culpritRelease = $derived.by(() => {
		const top = topBlockers[0]?.[0];
		if (!top) return null;
		const item = items.find((candidate) => candidate.reason_code === top);
		return item ? item.bundle_ordinal + 1 : null;
	});
	const applyCopy = $derived(managementApplyCopy(preview));
	const canApply = $derived(
		Boolean(
			preview?.ready_for_confirmation &&
			!activationPreview &&
			!preview.stale &&
			!preview.expired &&
			preview.summary.eligible_count + preview.summary.warning_count > 0 &&
			token.value
		)
	);
	const identityBlockerCount = $derived(
		(preview?.summary.reasons.TRACK_NOT_MAPPED ?? 0) +
			(preview?.summary.reasons.RELEASE_NOT_SELECTED ?? 0)
	);
	const applyDisabledReason = $derived.by((): string | null => {
		if (canApply || activationPreview || preview?.state !== 'ready') return null;
		if (preview.stale)
			return `Apply is disabled: this preview is stale (${preview.stale_reasons.map((code) => qualifiedManagementReasonLabel(code)).join(' · ') || 'inputs changed'}). Generate a fresh preview.`;
		if (preview.expired)
			return 'Apply is disabled: this preview expired. Generate a fresh preview.';
		if (!preview.ready_for_confirmation)
			return 'Apply is disabled: this preview is not awaiting confirmation.';
		if (applyCount === 0) return 'Apply is disabled: no eligible files and no files with warnings.';
		if (!token.value)
			return token.failed
				? 'Apply is disabled: the private apply token is missing and resuming it failed.'
				: 'Apply is disabled: resuming your private apply token.';
		return 'Apply is disabled for this preview.';
	});

	onMount(() => {
		const events = createLibraryManagementEvents();
		events.start();
		return events.stop;
	});

	$effect(() => {
		const element = stickyFooterElement;
		if (!element) {
			stickyFooterHeight = 0;
			return;
		}
		const updateHeight = () => {
			stickyFooterHeight = Math.ceil(element.getBoundingClientRect().height);
		};
		updateHeight();
		const observer = new ResizeObserver(updateHeight);
		observer.observe(element);
		return () => observer.disconnect();
	});

	function eligibilityTone(value: ManagementEligibility): ManagementAuditTone {
		return value === 'eligible' ? 'success' : value === 'warning' ? 'warning' : 'error';
	}

	function planAuditEntry(item: LibraryManagementPlanItem): ManagementAuditEntry {
		const artworkHash = item.artwork_choices
			.map(managementArtworkPreviewHash)
			.find((value): value is string => Boolean(value));
		return {
			ordinal: item.ordinal,
			bundleOrdinal: item.bundle_ordinal,
			trackLabel: managementPlanTrackLabel(item),
			title: managementPlanTitle(item),
			artist: managementPlanArtist(item),
			albumTitle: managementPlanAlbum(item),
			albumArtist: managementPlanAlbumArtist(item),
			albumId: item.local_album_id,
			albumMbid: managementDesiredField(item, 'musicbrainz_release_group_id'),
			albumArtworkVersion: managementAlbumArtworkVersion(item),
			format: managementAudioFormat(item),
			status: titleManagementValue(item.eligibility),
			statusTone: eligibilityTone(item.eligibility),
			reason: item.reason_code
				? qualifiedManagementReasonLabel(item.reason_code, firstDeferredSource(item))
				: null,
			reasonCode: item.reason_code,
			changes: managementPlanChanges(item),
			exceptional: managementPlanIsExceptional(item),
			sourceRoot: managementRootLabel(roots, item.source_root_id),
			sourcePath: item.source_relative_path,
			destinationRoot: managementRootLabel(roots, item.destination_root_id),
			destinationPath: item.destination_relative_path,
			artworkUrl: artworkHash
				? LIBRARY_MANAGEMENT_ENDPOINTS.previewArtwork(jobId, item.ordinal, artworkHash)
				: null
		};
	}
</script>

<svelte:head><title>Organization preview · DroppedNeedle</title></svelte:head>

{#snippet previewInspector(ordinal: number)}
	{@const item = items.find((candidate) => candidate.ordinal === ordinal)}
	{#if item}
		<LibraryManagementPlanInspector
			{item}
			{jobId}
			{roots}
			reasonLabel={(code) => qualifiedManagementReasonLabel(code)}
			onresolve={(item, collision, opener) => collisionDialog?.open(item, collision, opener)}
		/>
	{/if}
{/snippet}

<div class="management-preview-shell px-4 py-8 sm:px-6 lg:px-8">
	<main class="mx-auto max-w-7xl space-y-5">
		<BackButton fallback={withBasePath('/library/management?tab=organize')} />

		{#if previewQuery.isLoading || settingsQuery.isLoading || policyQuery.isLoading}
			<div class="space-y-4" role="status" aria-label="Loading organization preview">
				<div class="skeleton h-40 rounded-2xl"></div>
				<div class="skeleton h-72 rounded-2xl"></div>
			</div>
		{:else if previewQuery.isError || settingsQuery.isError || policyQuery.isError}
			<div class="alert alert-error" role="alert">Could not load this Organization preview.</div>
		{:else if preview}
			<header class="management-control-room p-5 sm:p-7">
				<div class="flex flex-wrap items-start gap-4">
					<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
					<div class="min-w-0 flex-1">
						<p class="management-kicker">
							<ShieldAlert class="h-3.5 w-3.5" /> Applying is the first write action · no files changed
						</p>
						<h1 class="mt-1 font-display text-2xl font-bold sm:text-3xl">
							{managementPreviewHeading(preview.mode)}
						</h1>
						<p class="mt-2 text-sm text-base-content/60">
							{preview.profile_name} · {titleManagementValue(preview.origin)} · created {formatManagementDate(
								preview.created_at
							)}
						</p>
					</div>
					<span
						class="badge badge-lg {preview.ready_for_confirmation
							? 'badge-success'
							: 'badge-outline'}"
						>{preview.terminal_code === 'PREVIEW_DISCARDED'
							? 'Discarded'
							: titleManagementValue(preview.phase)}</span
					>
				</div>
				<div class="mt-5 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
					<div class="management-summary-card">
						<span class="text-xs text-base-content/50">Eligible to write</span><strong
							>{preview.summary.eligible_count + preview.summary.warning_count}</strong
						><small>{preview.summary.warning_count} with warnings</small>
					</div>
					<div class="management-summary-card">
						<span class="text-xs text-base-content/50">Blocked / unchanged</span><strong
							>{preview.summary.blocked_count} / {preview.summary.no_change_count}</strong
						><small>Never implicitly included</small>
					</div>
					<div class="management-summary-card">
						<span class="text-xs text-base-content/50">Bundles / files</span><strong
							>{preview.summary.bundle_count} / {preview.summary.item_count}</strong
						><small>{preview.summary.expanded_track_count} tracks added by expansion</small>
					</div>
					<div class="management-summary-card">
						<span class="text-xs text-base-content/50">Temporary disk</span><strong
							>{formatManagementBytes(preview.summary.estimated_temporary_bytes)}</strong
						><small>Required for staging and recovery</small>
					</div>
				</div>
				<div class="mt-3 flex flex-wrap gap-2 text-xs">
					<span class="badge badge-outline"
						><Tags class="h-3 w-3" />
						{managementQuantity(preview.summary.tag_change_count, 'tag change')}</span
					>
					<span class="badge badge-outline"
						><Sparkles class="h-3 w-3" />
						{managementQuantity(preview.summary.artwork_change_count, 'artwork change')}</span
					>
					<span class="badge badge-outline"
						><FolderCog class="h-3 w-3" />
						{managementQuantity(preview.summary.path_change_count, 'path change')}</span
					>
					<span class="badge badge-outline"
						><Layers class="h-3 w-3" />
						{managementQuantity(preview.summary.sidecar_change_count, 'sidecar change')}</span
					>
					<span class="badge badge-outline"
						><Clock3 class="h-3 w-3" /> Expires {formatManagementDate(preview.expires_at)}</span
					>
					<span class="badge badge-outline"
						><HardDrive class="h-3 w-3" /> {managementProviderStatus(preview)}</span
					>
				</div>
			</header>

			{#if preview.stale || preview.expired}
				<div class="alert alert-error items-start" role="alert">
					<ShieldAlert class="mt-0.5 h-5 w-5" /><span
						><strong>This preview cannot be applied.</strong><br />{preview.expired
							? 'It expired. Generate a fresh preview.'
							: managementStaleBannerDetail(preview)}</span
					>
				</div>
			{:else if preview.state === 'failed'}
				<div class="alert alert-error items-start" role="alert">
					<ShieldAlert class="mt-0.5 h-5 w-5" /><span
						><strong>Preview planning failed.</strong><br />{preview.terminal_code
							? titleManagementValue(preview.terminal_code)
							: 'Generate a fresh preview and try again.'} No files were changed.</span
					>
				</div>
			{:else if ['queued', 'running', 'paused'].includes(preview.state)}
				<div class="alert alert-info">
					<span class="loading loading-spinner loading-sm"></span><span
						>Planning is still read-only. {preview.summary.item_count > 0
							? `${preview.summary.item_count.toLocaleString()} files are planned so far; the total is discovered as DroppedNeedle works.`
							: 'DroppedNeedle is discovering the files in this scope.'}</span
					>
				</div>
			{:else if preview.state !== 'ready'}
				<div class="alert alert-info items-start">
					<ShieldAlert class="mt-0.5 h-5 w-5" /><span
						><strong>This preview is no longer awaiting confirmation.</strong><br
						/>{preview.terminal_code
							? titleManagementValue(preview.terminal_code)
							: titleManagementValue(preview.state)}. No further write can start from this page.</span
					>
				</div>
			{/if}

			{#if zeroAppliable}
				<div class="alert alert-warning items-start" role="status">
					<ShieldAlert class="mt-0.5 h-5 w-5" />
					<div class="min-w-0 flex-1">
						<strong>Nothing in this preview can be applied.</strong>
						{#if topBlockers.length}
							<p class="mt-1 text-sm">
								Top blockers: {topBlockers
									.map(
										([code, count]) =>
											`${qualifiedManagementReasonLabel(code)} (${count.toLocaleString()})`
									)
									.join(' · ')}
							</p>
						{/if}
						{#if culpritRelease !== null && topBlockers[0]}
							<p class="mt-1 text-sm">
								Start with Release {culpritRelease}: its blocked rows name the exact cause.
								<button
									type="button"
									class="link link-hover"
									onclick={() => {
										filters.reasonCode = topBlockers[0][0];
									}}>Filter to {qualifiedManagementReasonLabel(topBlockers[0][0])}</button
								>
							</p>
						{/if}
						{#if identityBlockerCount > 0}
							<p class="mt-1 text-sm">
								Selecting a root chooses files; it does not choose each release's exact MusicBrainz
								edition. Prepare identities first, then generate a fresh management preview.
							</p>
							<Button
								variant="outline"
								size="sm"
								class="mt-3"
								icon={ArrowRight}
								iconPosition="end"
								href={withBasePath('/library/management?tab=organize')}
								>Open identity readiness</Button
							>
						{/if}
					</div>
				</div>
			{:else if preview.state === 'ready' && identityBlockerCount > 0}
				<div class="alert alert-warning items-start">
					<BookOpenCheck class="mt-0.5 h-5 w-5" />
					<div class="min-w-0 flex-1">
						<strong>{identityBlockerCount.toLocaleString()} files need identity preparation.</strong
						>
						<p class="mt-1 text-sm">
							Selecting a root chooses files; it does not choose each release's exact MusicBrainz
							edition. Prepare identities first, then generate a fresh management preview.
						</p>
						<Button
							variant="outline"
							size="sm"
							class="mt-3"
							icon={ArrowRight}
							iconPosition="end"
							href={withBasePath('/library/management?tab=organize')}
							>Open identity readiness</Button
						>
					</div>
				</div>
			{/if}

			{#if deferredSources.length}
				<div class="alert alert-info items-start" role="status">
					<Sparkles class="mt-0.5 h-5 w-5" />
					<div class="min-w-0 flex-1">
						<strong>Optional enrichment deferred · files planned without it.</strong>
						<p class="mt-1 text-sm">
							Deferred warnings: {deferredSources
								.map(([source, count]) => `${source} (${count.toLocaleString()})`)
								.join(' · ')}
						</p>
					</div>
				</div>
			{/if}

			<LibraryManagementPlanFilterBar {filters} {preview} {roots} />

			{#if itemsQuery.isLoading}
				<div class="space-y-3" role="status" aria-label="Loading preview files">
					<div class="skeleton h-28 rounded-xl"></div>
					<div class="skeleton h-28 rounded-xl"></div>
				</div>
			{:else if itemsQuery.isError}
				<div class="alert alert-error" role="alert">Could not load preview items.</div>
			{:else if items.length === 0}
				<div
					class="rounded-2xl border border-dashed border-base-content/15 p-8 text-center text-base-content/50"
				>
					No files match these filters.
					<div class="mt-3">
						<Button variant="ghost" size="sm" onclick={() => filters.clear()}>Clear filters</Button>
					</div>
				</div>
			{:else}
				<LibraryManagementAuditDossiers
					dossiers={auditDossiers}
					inspector={previewInspector}
					detailLabel="Inspect exact diff"
					reserveStickyFooter={preview.state === 'ready'}
					{stickyFooterHeight}
					incomplete={Boolean(itemsQuery.hasNextPage)}
				/>
				{#if itemsQuery.hasNextPage}<Button
						variant="outline"
						class="w-full"
						loading={itemsQuery.isFetchingNextPage}
						onclick={() => void itemsQuery.fetchNextPage()}>Load more files</Button
					>{/if}
			{/if}

			{#if preview.state === 'ready'}
				{#if activationPreview}<div
						bind:this={stickyFooterElement}
						class="management-apply-bar"
						data-testid="management-preview-sticky-footer"
					>
						<div class="flex items-start gap-2">
							<ShieldAlert class="mt-0.5 h-5 w-5 text-library-manage" />
							<div>
								<strong>Activation dry run</strong>
								<p class="text-xs text-base-content/55">
									This page is read-only. Return to the Automation tab to use this dry run when
									enabling file organization.
								</p>
							</div>
						</div>
						<div class="flex flex-wrap items-center gap-1">
							<LibraryManagementDiscardPreview
								{jobId}
								expectedRevision={preview.operation_row_revision}
								profileName={preview.profile_name}
								ondiscard={() => goto(withBasePath('/library/management?tab=organize'))}
							/>
							<Button variant="ghost" size="sm" href={withBasePath('/settings?tab=library')}
								>Library settings</Button
							>
						</div>
					</div>{:else}<div
						bind:this={stickyFooterElement}
						class="management-apply-bar"
						data-testid="management-preview-sticky-footer"
					>
						<div class="flex items-start gap-2">
							<ShieldAlert class="mt-0.5 h-5 w-5 text-library-manage" />
							<div>
								<strong>{applyCopy.barTitle}</strong>
								<p class="text-xs text-base-content/55">{applyCopy.barDetail}</p>
								{#if applyDisabledReason}
									<p class="mt-1 text-xs text-warning">{applyDisabledReason}</p>
								{/if}
								{#if !token.value && preview.ready_for_confirmation}
									{#if token.failed}
										<Button variant="ghost" size="xs" class="mt-1" onclick={() => token.retry()}>
											Retry token resume
										</Button>
									{:else if token.resuming}
										<p class="mt-1 text-xs text-base-content/55">
											Resuming your private apply token…
										</p>
									{/if}
								{/if}
							</div>
						</div>
						<div class="flex flex-wrap items-center gap-1">
							<LibraryManagementDiscardPreview
								{jobId}
								expectedRevision={preview.operation_row_revision}
								profileName={preview.profile_name}
								ondiscard={() => goto(withBasePath('/library/management?tab=organize'))}
							/>
							<Button
								variant="warning"
								disabled={!canApply}
								onclick={(event) => applyDialog?.open(event.currentTarget)}
								>{applyCopy.button}</Button
							>
						</div>
					</div>{/if}
			{/if}
		{/if}
	</main>
</div>

{#if preview}
	<LibraryManagementApplyDialog
		bind:this={applyDialog}
		{jobId}
		{preview}
		token={token.value}
		{canApply}
		copy={applyCopy}
	/>
	<LibraryManagementCollisionDialog
		bind:this={collisionDialog}
		{jobId}
		{preview}
		{roots}
		settingsRevision={settingsQuery.data?.settings_revision ?? null}
		policyRevision={policyQuery.data?.policy_revision ?? null}
		recycleAvailable={Boolean(settingsQuery.data?.recycle_bin_path.trim())}
	/>
{/if}

<style>
	.management-apply-bar {
		position: sticky;
		bottom: 0.75rem;
		z-index: 10;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.85rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 32%, transparent);
		border-radius: 1rem;
		background: color-mix(in oklab, var(--color-base-100) 92%, transparent);
		box-shadow: 0 1rem 2.5rem color-mix(in oklab, var(--color-base-content) 15%, transparent);
		backdrop-filter: blur(12px);
	}

	@media (max-width: 639px) {
		.management-apply-bar {
			align-items: stretch;
			flex-direction: column;
		}
	}
</style>
