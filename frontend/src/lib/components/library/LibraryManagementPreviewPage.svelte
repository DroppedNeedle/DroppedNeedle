<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import {
		ArrowRight,
		BookOpenCheck,
		CheckCircle2,
		Clock3,
		FolderCog,
		HardDrive,
		Layers3,
		ShieldAlert,
		Sparkles,
		Tags,
		X
	} from 'lucide-svelte';

	import BackButton from '$lib/components/BackButton.svelte';
	import LibraryManagementDiscardPreview from './LibraryManagementDiscardPreview.svelte';
	import { API } from '$lib/constants';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { getLibrarySearchQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { createLibraryManagementEvents } from '$lib/queries/library-management/LibraryManagementEvents';
	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/queries/library-management/LibraryManagementConfirmation';
	import {
		applyLibraryManagementPreviewMutation,
		createLibraryManagementDuplicateResolutionMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import {
		getLibraryManagementPlanItemsQuery,
		getLibraryManagementPreviewQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import {
		forgetLibraryManagementPreviewToken,
		readLibraryManagementPreviewToken,
		rememberLibraryManagementPreviewToken
	} from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import type {
		DuplicateResolutionAction,
		LibraryManagementPlanItem,
		ManagementChangeKind,
		ManagementEligibility
	} from '$lib/queries/library-management/types';
	import { createUuid } from '$lib/utils/uuid';
	import {
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
		titleManagementValue,
		type ManagementCollision
	} from './LibraryManagementDisplay';
	import LibraryManagementAuditDossiers from './LibraryManagementAuditDossiers.svelte';
	import LibraryManagementPlanInspector from './LibraryManagementPlanInspector.svelte';
	import {
		groupManagementAuditEntries,
		type ManagementAuditEntry,
		type ManagementAuditTone
	} from './LibraryManagementAuditTypes';

	interface Props {
		jobId: string;
	}

	interface CollisionSelection {
		item: LibraryManagementPlanItem;
		collision: ManagementCollision;
	}

	interface ApplyActionCopy {
		barTitle: string;
		barDetail: string;
		button: string;
		kicker: string;
		title: string;
		detail: string;
		confirmButton: string;
	}
	let { jobId }: Props = $props();
	let eligibility = $state<ManagementEligibility | ''>('');
	let reasonCode = $state('');
	let rootId = $state('');
	let artistId = $state('');
	let artistLabel = $state('');
	let albumId = $state('');
	let albumLabel = $state('');
	let catalogSearch = $state('');
	let audioFormat = $state('');
	let changeKind = $state<ManagementChangeKind | ''>('');
	let collisionClass = $state('');
	let hasPreservedValue = $state(false);
	let hasRepresentationLoss = $state(false);
	let previewToken = $derived(readLibraryManagementPreviewToken(jobId));
	let confirmation = $state('');
	let applyError = $state('');
	let applyDialog: HTMLDialogElement;
	let applyHeading: HTMLHeadingElement;
	let applyOpener: HTMLButtonElement | null = null;
	let collisionDialog: HTMLDialogElement;
	let collisionHeading: HTMLHeadingElement;
	let collisionOpener: HTMLButtonElement | null = null;
	let collisionSelection = $state<CollisionSelection | null>(null);
	let collisionAction = $state<DuplicateResolutionAction | ''>('');
	let alternateRelativePath = $state('');
	let collisionError = $state('');
	let stickyFooterElement: HTMLElement | null = $state(null);
	let stickyFooterHeight = $state(0);

	const previewQuery = getLibraryManagementPreviewQuery(
		() => authStore.user?.id,
		() => jobId
	);
	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const policyQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const catalogSearchQuery = getLibrarySearchQuery(() => catalogSearch);
	const itemsQuery = getLibraryManagementPlanItemsQuery(
		() => authStore.user?.id,
		() => jobId,
		() => ({
			limit: 50,
			eligibility: eligibility || undefined,
			reasonCode: reasonCode || undefined,
			rootId: rootId || undefined,
			artistId: artistId || undefined,
			albumId: albumId || undefined,
			audioFormat: audioFormat || undefined,
			collisionClass: collisionClass || undefined,
			hasPreservedValue: hasPreservedValue || undefined,
			hasRepresentationLoss: hasRepresentationLoss || undefined,
			changeKind: changeKind || undefined
		})
	);
	const applyPreview = applyLibraryManagementPreviewMutation();
	const createResolution = createLibraryManagementDuplicateResolutionMutation();

	const preview = $derived(previewQuery.data ?? null);
	const items = $derived(itemsQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const roots = $derived(policyQuery.data?.library_roots ?? []);
	const auditDossiers = $derived(
		groupManagementAuditEntries(items.map((item) => planAuditEntry(item)))
	);
	const applyCount = $derived(
		(preview?.summary.eligible_count ?? 0) + (preview?.summary.warning_count ?? 0)
	);
	const applyFileLabel = $derived(`${applyCount} ${applyCount === 1 ? 'file' : 'files'}`);
	const applyAction = $derived.by<ApplyActionCopy>(() => {
		switch (preview?.mode) {
			case 'undo':
				return {
					barTitle: 'Undo changes only after confirmation',
					barDetail: 'Files changed again later, blocked rows, and expired snapshots are excluded.',
					button: `Undo this operation for ${applyFileLabel}`,
					kicker: 'Undo confirmation',
					title: 'Undo this operation from this exact preview?',
					detail: `DroppedNeedle will restore this operation's before-state for ${applyFileLabel}. Later edits are preserved and this does not restore the broader original baseline.`,
					confirmButton: 'Undo operation'
				};
			case 'baseline_restore':
				return {
					barTitle: 'Baseline restore changes files only after confirmation',
					barDetail:
						'Restored files leave file organization and can be managed again only through a new preview.',
					button: `Restore original state for ${applyFileLabel}`,
					kicker: 'Original baseline confirmation',
					title: 'Restore these original baselines?',
					detail: `DroppedNeedle will restore ${applyFileLabel} to their earliest saved state from before it first managed them. This is broader than Undo and leaves those files unmanaged.`,
					confirmButton: 'Restore original state'
				};
			case 'duplicate_resolution':
				return {
					barTitle: 'Collision resolution changes files only after confirmation',
					barDetail:
						'Only the explicitly selected resolution is included; no duplicate is deleted automatically.',
					button: `Apply collision resolution for ${applyFileLabel}`,
					kicker: 'Collision-resolution confirmation',
					title: 'Apply this exact collision resolution?',
					detail: `DroppedNeedle will carry out the explicitly previewed collision resolution for ${applyFileLabel}. No destination is overwritten and no duplicate is deleted automatically.`,
					confirmButton: 'Apply collision resolution'
				};
			default:
				return {
					barTitle: 'Applying is the first write action',
					barDetail: 'Blocked, stale, and no-change rows are excluded.',
					button: `Write tags and organize ${applyFileLabel}`,
					kicker: 'Write confirmation',
					title: 'Apply this exact preview?',
					detail: `DroppedNeedle will write tags and organize ${preview?.summary.eligible_count ?? 0} eligible files plus ${preview?.summary.warning_count ?? 0} files with warnings. No destination is overwritten automatically.`,
					confirmButton: 'Apply exact preview'
				};
		}
	});
	const activationPreview = $derived(
		Boolean(preview && preview.proposed_settings_revision !== null)
	);
	const canApply = $derived(
		Boolean(
			preview?.ready_for_confirmation &&
			!activationPreview &&
			!preview.stale &&
			!preview.expired &&
			preview.summary.eligible_count + preview.summary.warning_count > 0 &&
			previewToken
		)
	);
	const recycleAvailable = $derived(Boolean(settingsQuery.data?.recycle_bin_path.trim()));
	const providerStatus = $derived(
		preview?.summary.reasons.METADATA_UNAVAILABLE
			? 'Required metadata unavailable'
			: preview?.summary.reasons.OPTIONAL_ENRICHMENT_DEFERRED
				? 'Optional enrichment deferred'
				: 'Required metadata pinned'
	);
	const identityBlockerCount = $derived(
		(preview?.summary.reasons.TRACK_NOT_MAPPED ?? 0) +
			(preview?.summary.reasons.RELEASE_NOT_SELECTED ?? 0)
	);
	const collisionRequestReady = $derived(
		Boolean(
			collisionSelection?.collision.requestKind &&
			collisionSelection.collision.existingRootId &&
			collisionSelection.collision.existingRelativePath &&
			collisionAction &&
			(!collisionAction.startsWith('recycle_') || recycleAvailable) &&
			(collisionAction !== 'keep_incoming_alternate' || alternateRelativePath.trim())
		)
	);

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

	function rootLabel(value: string | null): string {
		return (
			roots.find((root) => root.id === value)?.label ?? (value ? 'Unavailable root' : 'No root')
		);
	}

	function displayPath(root: string | null, relative: string | null): string {
		return `${rootLabel(root)} · ${relative ?? 'No path'}`;
	}

	function managementReasonLabel(value: string): string {
		return (
			{
				TRACK_NOT_MAPPED: 'Exact edition selected; track map missing',
				RELEASE_NOT_SELECTED: 'Exact MusicBrainz edition not chosen',
				FILE_UNREADABLE: 'File metadata could not be read',
				PATH_TOO_LONG: 'Planned path exceeds the configured length limit',
				SCRIPT_VALIDATION_FAILED: 'Profile script could not safely process this file'
			}[value] ?? titleManagementValue(value)
		);
	}

	function eligibilityTone(value: ManagementEligibility): ManagementAuditTone {
		return value === 'eligible' ? 'success' : value === 'warning' ? 'warning' : 'error';
	}

	function planAuditEntry(item: LibraryManagementPlanItem): ManagementAuditEntry {
		const artworkUrl = item.artwork_choices
			.map((choice) => {
				const sha256 = managementArtworkPreviewHash(choice);
				return sha256 ? API.libraryManagement.previewArtwork(jobId, item.ordinal, sha256) : null;
			})
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
			reason: item.reason_code ? managementReasonLabel(item.reason_code) : null,
			changes: managementPlanChanges(item),
			exceptional: managementPlanIsExceptional(item),
			sourceRoot: rootLabel(item.source_root_id),
			sourcePath: item.source_relative_path,
			destinationRoot: rootLabel(item.destination_root_id),
			destinationPath: item.destination_relative_path,
			artworkUrl: artworkUrl ?? null
		};
	}

	function previewHeading(mode: string): string {
		return mode === 'preview' ? 'Organization preview' : `${titleManagementValue(mode)} preview`;
	}

	function quantity(value: number, singular: string, plural = `${singular}s`): string {
		return `${value.toLocaleString()} ${value === 1 ? singular : plural}`;
	}

	function formatBytes(value: number): string {
		if (value < 1024) return `${value} B`;
		if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KiB`;
		if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MiB`;
		return `${(value / 1024 ** 3).toFixed(1)} GiB`;
	}

	function formatDate(value: number | null): string {
		return value ? new Date(value * 1000).toLocaleString() : 'No expiry';
	}

	function chooseArtist(id: string, label: string): void {
		artistId = id;
		artistLabel = label;
		catalogSearch = '';
	}

	function chooseAlbum(id: string, label: string): void {
		albumId = id;
		albumLabel = label;
		catalogSearch = '';
	}

	function openApply(opener: HTMLButtonElement): void {
		applyOpener = opener;
		confirmation = '';
		applyError = '';
		applyDialog.showModal();
		applyHeading.focus();
	}

	async function apply(): Promise<void> {
		if (
			!preview ||
			!previewToken ||
			confirmation !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
			!canApply
		)
			return;
		applyError = '';
		try {
			const operation = await applyPreview.mutateAsync({
				jobId,
				request: {
					preview_token: previewToken,
					expected_operation_row_revision: preview.operation_row_revision,
					idempotency_key: createUuid(),
					confirmation: true
				}
			});
			forgetLibraryManagementPreviewToken(jobId);
			applyDialog.close();
			await goto(resolve(`/library/management/operations/${encodeURIComponent(operation.id)}`));
		} catch (error) {
			applyError = error instanceof Error ? error.message : 'Could not apply this preview.';
		}
	}

	function openCollision(
		item: LibraryManagementPlanItem,
		collision: ManagementCollision,
		opener: HTMLButtonElement
	): void {
		collisionOpener = opener;
		collisionSelection = { item, collision };
		collisionAction = '';
		alternateRelativePath = '';
		collisionError = '';
		collisionDialog.showModal();
		collisionHeading.focus();
	}

	async function resolveCollision(): Promise<void> {
		const selection = collisionSelection;
		const settings = settingsQuery.data;
		const policy = policyQuery.data;
		if (
			!selection?.collision.requestKind ||
			!selection.collision.existingRootId ||
			!selection.collision.existingRelativePath ||
			!collisionAction ||
			!settings ||
			!policy ||
			!preview ||
			!collisionRequestReady
		) {
			return;
		}
		collisionError = '';
		try {
			const handle = await createResolution.mutateAsync({
				source_job_id: jobId,
				source_plan_item_ordinal: selection.item.ordinal,
				expected_source_operation_row_revision: preview.operation_row_revision,
				collision_kind: selection.collision.requestKind,
				existing_root_id: selection.collision.existingRootId,
				existing_relative_path: selection.collision.existingRelativePath,
				action: collisionAction,
				expected_settings_revision: settings.settings_revision,
				expected_policy_revision: policy.policy_revision,
				idempotency_key: createUuid(),
				existing_local_track_id: selection.collision.existingLocalTrackId,
				alternate_relative_path:
					collisionAction === 'keep_incoming_alternate' ? alternateRelativePath.trim() : null
			});
			rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
			collisionDialog.close();
			await goto(resolve(`/library/management/previews/${encodeURIComponent(handle.job_id)}`));
		} catch (error) {
			collisionError =
				error instanceof Error ? error.message : 'Could not create a resolution preview.';
		}
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
			reasonLabel={managementReasonLabel}
			onresolve={openCollision}
		/>
	{/if}
{/snippet}

<div class="management-preview-shell px-4 py-8 sm:px-6 lg:px-8">
	<main class="mx-auto max-w-7xl space-y-5">
		<BackButton fallback="/library/management?tab=organize" />

		{#if previewQuery.isLoading || settingsQuery.isLoading || policyQuery.isLoading}
			<div class="space-y-4">
				<div class="skeleton h-40 rounded-2xl"></div>
				<div class="skeleton h-72 rounded-2xl"></div>
			</div>
		{:else if previewQuery.isError || settingsQuery.isError || policyQuery.isError}
			<div class="alert alert-error">Could not load this Organization preview.</div>
		{:else if preview}
			<header class="management-control-room p-5 sm:p-7">
				<div class="flex flex-wrap items-start gap-4">
					<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
					<div class="min-w-0 flex-1">
						<p class="management-kicker">
							<ShieldAlert class="h-3.5 w-3.5" /> Read-only plan · no files changed
						</p>
						<h1 class="mt-1 font-display text-2xl font-bold sm:text-3xl">
							{previewHeading(preview.mode)}
						</h1>
						<p class="mt-2 text-sm text-base-content/60">
							{preview.profile_name} · {titleManagementValue(preview.origin)} · created {formatDate(
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
							>{formatBytes(preview.summary.estimated_temporary_bytes)}</strong
						><small>Required for staging and recovery</small>
					</div>
				</div>
				<div class="mt-3 flex flex-wrap gap-2 text-xs">
					<span class="badge badge-outline"
						><Tags class="h-3 w-3" />
						{quantity(preview.summary.tag_change_count, 'tag change')}</span
					>
					<span class="badge badge-outline"
						><Sparkles class="h-3 w-3" />
						{quantity(preview.summary.artwork_change_count, 'artwork change')}</span
					>
					<span class="badge badge-outline"
						><FolderCog class="h-3 w-3" />
						{quantity(preview.summary.path_change_count, 'path change')}</span
					>
					<span class="badge badge-outline"
						><Layers3 class="h-3 w-3" />
						{quantity(preview.summary.sidecar_change_count, 'sidecar change')}</span
					>
					<span class="badge badge-outline"
						><Clock3 class="h-3 w-3" /> Expires {formatDate(preview.expires_at)}</span
					>
					<span class="badge badge-outline"><HardDrive class="h-3 w-3" /> {providerStatus}</span>
				</div>
			</header>

			{#if preview.stale || preview.expired}
				<div class="alert alert-error items-start">
					<ShieldAlert class="mt-0.5 h-5 w-5" /><span
						><strong>This preview cannot be applied.</strong><br />{preview.expired
							? 'It expired. Generate a fresh preview.'
							: preview.stale_reasons.map(titleManagementValue).join(' · ')}</span
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

			{#if preview.state === 'ready' && identityBlockerCount > 0}
				<div class="alert alert-warning items-start">
					<BookOpenCheck class="mt-0.5 h-5 w-5" />
					<div class="min-w-0 flex-1">
						<strong>{identityBlockerCount.toLocaleString()} files need identity preparation.</strong
						>
						<p class="mt-1 text-sm">
							Selecting a root chooses files; it does not choose each release's exact MusicBrainz
							edition. Prepare identities first, then generate a fresh management preview.
						</p>
						<a
							class="btn btn-outline btn-sm mt-3"
							href={resolve('/library/management?tab=organize')}
							>Open identity readiness <ArrowRight class="h-4 w-4" /></a
						>
					</div>
				</div>
			{/if}

			<section class="management-operation-panel space-y-4" aria-labelledby="preview-filters-title">
				<div class="flex flex-wrap items-end justify-between gap-2">
					<div>
						<p class="management-step">Inspectable plan</p>
						<h2 id="preview-filters-title" class="font-display text-xl font-semibold">
							Files and changes
						</h2>
					</div>
					<span class="text-xs text-base-content/45">Root labels and relative paths only</span>
				</div>
				<div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
					<label class="grid gap-1 text-xs"
						><span>Outcome</span><select
							class="select select-bordered select-sm bg-base-100"
							bind:value={eligibility}
							><option value="">All outcomes</option><option value="eligible">Eligible</option
							><option value="warning">Warning</option><option value="blocked">Blocked</option
							><option value="stale">Stale</option></select
						></label
					>
					<label class="grid gap-1 text-xs"
						><span>Reason</span><select
							class="select select-bordered select-sm bg-base-100"
							bind:value={reasonCode}
							><option value="">All reasons</option
							>{#each Object.keys(preview.summary.reasons) as reason (reason)}<option value={reason}
									>{managementReasonLabel(reason)}</option
								>{/each}</select
						></label
					>
					<label class="grid gap-1 text-xs"
						><span>Root</span><select
							class="select select-bordered select-sm bg-base-100"
							bind:value={rootId}
							><option value="">All roots</option
							>{#each Object.keys(preview.summary.roots) as root (root)}<option value={root}
									>{rootLabel(root)}</option
								>{/each}</select
						></label
					>
					<label class="grid gap-1 text-xs"
						><span>Format</span><select
							class="select select-bordered select-sm bg-base-100"
							bind:value={audioFormat}
							><option value="">All formats</option
							>{#each Object.keys(preview.summary.formats) as format (format)}<option value={format}
									>{format.toUpperCase()}</option
								>{/each}</select
						></label
					>
					<label class="grid gap-1 text-xs"
						><span>Change</span><select
							class="select select-bordered select-sm bg-base-100"
							bind:value={changeKind}
							><option value="">All changes</option><option value="tags">Tags</option><option
								value="artwork">Artwork</option
							><option value="path">Path</option><option value="sidecars">Sidecars</option><option
								value="no_change">No change</option
							></select
						></label
					>
				</div>
				<details class="rounded-xl border border-base-content/10 bg-base-200/35 p-3">
					<summary class="cursor-pointer text-sm font-semibold"
						>Artist, release, collision, and preservation filters</summary
					>
					<div class="mt-3 grid gap-3 lg:grid-cols-2">
						<div class="space-y-2">
							<label class="grid gap-1 text-xs"
								><span>Find an artist or release</span><input
									class="input input-bordered input-sm bg-base-100"
									bind:value={catalogSearch}
									placeholder="Type at least two characters"
								/></label
							>
							{#if artistId || albumId}<div class="flex flex-wrap gap-1">
									{#if artistId}<button
											class="badge badge-outline gap-1"
											onclick={() => {
												artistId = '';
												artistLabel = '';
											}}>Artist: {artistLabel} ×</button
										>{/if}{#if albumId}<button
											class="badge badge-outline gap-1"
											onclick={() => {
												albumId = '';
												albumLabel = '';
											}}>Release: {albumLabel} ×</button
										>{/if}
								</div>{/if}
							{#if catalogSearch.trim().length >= 2}
								{#if catalogSearchQuery.isLoading}<div
										class="skeleton h-16 rounded-xl"
									></div>{:else if catalogSearchQuery.isError}<div
										class="alert alert-error py-2 text-xs"
										role="alert"
									>
										Could not search artists and releases.
									</div>{:else if catalogSearchQuery.data && catalogSearchQuery.data.artists.length + catalogSearchQuery.data.albums.length === 0}<p
										class="rounded-xl border border-dashed border-base-content/15 p-3 text-xs text-base-content/50"
									>
										No matching artists or releases.
									</p>{:else if catalogSearchQuery.data}<div
										class="grid max-h-44 gap-1 overflow-y-auto rounded-xl border border-base-content/10 bg-base-100 p-2"
									>
										{#each catalogSearchQuery.data.artists as artist (artist.id)}<button
												class="btn btn-ghost btn-sm justify-start"
												onclick={() => chooseArtist(artist.id, artist.name)}
												>Artist · {artist.name}</button
											>{/each}{#each catalogSearchQuery.data.albums as album (album.id)}<button
												class="btn btn-ghost btn-sm justify-start"
												onclick={() =>
													chooseAlbum(album.id, `${album.artist_name} · ${album.title}`)}
												>Release · {album.artist_name} · {album.title}</button
											>{/each}
									</div>{/if}
							{/if}
						</div>
						<div class="grid gap-2 sm:grid-cols-2">
							<label class="grid gap-1 text-xs sm:col-span-2"
								><span>Collision class</span><select
									class="select select-bordered select-sm bg-base-100"
									bind:value={collisionClass}
									><option value="">All collision classes</option
									>{#each ['same_path_same_content', 'same_path_different_content', 'same_release_position_different_content', 'normalized_path_collision', 'normalized_catalog_path_collision', 'sidecar_path_collision', 'destination_created_after_preview'] as option (option)}<option
											value={option}>{titleManagementValue(option)}</option
										>{/each}</select
								></label
							>
							<label class="management-trigger"
								><input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={hasPreservedValue}
								/><span
									><strong>Preserved / local override</strong><small
										>Values intentionally left unchanged</small
									></span
								></label
							>
							<label class="management-trigger"
								><input
									type="checkbox"
									class="checkbox checkbox-sm"
									bind:checked={hasRepresentationLoss}
								/><span
									><strong>Lossy representation</strong><small
										>Format cannot store the exact value shape</small
									></span
								></label
							>
						</div>
					</div>
				</details>
			</section>

			{#if itemsQuery.isLoading}
				<div class="space-y-3">
					<div class="skeleton h-28 rounded-xl"></div>
					<div class="skeleton h-28 rounded-xl"></div>
				</div>
			{:else if itemsQuery.isError}
				<div class="alert alert-error">Could not load preview items.</div>
			{:else if items.length === 0}
				<div
					class="rounded-2xl border border-dashed border-base-content/15 p-8 text-center text-base-content/50"
				>
					No files match these filters.
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
				{#if itemsQuery.hasNextPage}<button
						class="btn btn-outline w-full"
						disabled={itemsQuery.isFetchingNextPage}
						onclick={() => void itemsQuery.fetchNextPage()}
						>{#if itemsQuery.isFetchingNextPage}<span class="loading loading-spinner loading-sm"
							></span>{/if} Load more files</button
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
								ondiscard={() => goto(resolve('/library/management?tab=organize'))}
							/>
							<a href={resolve('/settings?tab=library')} class="btn btn-ghost btn-sm"
								>Library settings</a
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
								<strong>{applyAction.barTitle}</strong>
								<p class="text-xs text-base-content/55">{applyAction.barDetail}</p>
								{#if !previewToken && preview.ready_for_confirmation}<p
										class="mt-1 text-xs text-warning"
									>
										The private apply token is not in this browser session. Generate a fresh preview
										to apply.
									</p>{/if}
							</div>
						</div>
						<div class="flex flex-wrap items-center gap-1">
							<LibraryManagementDiscardPreview
								{jobId}
								expectedRevision={preview.operation_row_revision}
								profileName={preview.profile_name}
								ondiscard={() => goto(resolve('/library/management?tab=organize'))}
							/>
							<button
								class="btn management-btn"
								disabled={!canApply}
								onclick={(event) => openApply(event.currentTarget)}>{applyAction.button}</button
							>
						</div>
					</div>{/if}
			{/if}
		{/if}
	</main>
</div>

<dialog
	bind:this={applyDialog}
	class="modal"
	aria-labelledby="apply-management-title"
	onclose={() => applyOpener?.focus()}
	oncancel={(event) => {
		if (applyPreview.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-lg border border-warning/30">
		<div class="flex items-start gap-3">
			<div class="management-write-mark"><ShieldAlert class="h-5 w-5" /></div>
			<div>
				<p class="management-kicker">{applyAction.kicker}</p>
				<h2
					bind:this={applyHeading}
					id="apply-management-title"
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					{applyAction.title}
				</h2>
			</div>
		</div>
		<p class="mt-4 text-sm text-base-content/65">{applyAction.detail}</p>
		<label class="mt-4 grid gap-1 text-sm"
			><span>Type <strong>{LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}</strong></span><input
				class="input input-bordered bg-base-100 font-mono"
				bind:value={confirmation}
				autocomplete="off"
			/></label
		>
		{#if applyError}<div class="alert alert-error mt-3 text-sm" role="alert">{applyError}</div>{/if}
		<div class="modal-action">
			<button
				class="btn btn-ghost"
				disabled={applyPreview.isPending}
				onclick={() => applyDialog.close()}>Cancel</button
			><button
				class="btn btn-warning"
				disabled={!canApply ||
					confirmation !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
					applyPreview.isPending}
				onclick={() => void apply()}
				>{#if applyPreview.isPending}<span class="loading loading-spinner loading-sm"
					></span>{/if}<CheckCircle2 class="h-4 w-4" />
				{applyAction.confirmButton}</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel applying management preview" disabled={applyPreview.isPending}
			>close</button
		>
	</form>
</dialog>

<dialog
	bind:this={collisionDialog}
	class="modal"
	aria-labelledby="resolve-management-collision"
	onclose={() => collisionOpener?.focus()}
	oncancel={(event) => {
		if (createResolution.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-2xl border border-error/25">
		<div class="flex items-start justify-between gap-3">
			<div>
				<p class="management-kicker">Fresh preview required</p>
				<h2
					bind:this={collisionHeading}
					id="resolve-management-collision"
					tabindex="-1"
					class="font-display text-xl font-semibold"
				>
					Choose a collision resolution
				</h2>
				<p class="mt-1 text-sm text-base-content/55">
					No option is preselected. DroppedNeedle rechecks both files before it offers another Apply
					action.
				</p>
			</div>
			<button
				class="btn btn-ghost btn-sm btn-square"
				aria-label="Close collision resolution"
				disabled={createResolution.isPending}
				onclick={() => collisionDialog.close()}><X class="h-5 w-5" /></button
			>
		</div>
		{#if collisionSelection}<div class="mt-4 rounded-xl border border-base-content/10 p-3 text-sm">
				<p>
					<strong>Incoming:</strong>
					{displayPath(
						collisionSelection.item.source_root_id,
						collisionSelection.item.source_relative_path
					)}
				</p>
				<p class="mt-1">
					<strong>Existing:</strong>
					{displayPath(
						collisionSelection.collision.existingRootId,
						collisionSelection.collision.existingRelativePath
					)}
				</p>
				<p class="mt-1">
					<strong>Evidence:</strong>
					{titleManagementValue(collisionSelection.collision.classification)}
				</p>
			</div>{/if}
		<fieldset class="mt-4 grid gap-2">
			<legend class="mb-2 text-sm font-semibold">Resolution</legend>
			<label class="management-selection-card"
				><input
					type="radio"
					class="radio radio-sm"
					name="collision-action"
					value="keep_existing"
					bind:group={collisionAction}
				/><span
					><strong>Keep existing; leave incoming in place</strong><small
						>Creates a no-write resolution preview for the incoming destination.</small
					></span
				></label
			>
			<label class="management-selection-card"
				><input
					type="radio"
					class="radio radio-sm"
					name="collision-action"
					value="keep_incoming_alternate"
					bind:group={collisionAction}
				/><span
					><strong>Keep incoming at an alternate relative path</strong><small
						>Both files remain.</small
					></span
				></label
			>
			<label class="management-selection-card"
				><input
					type="radio"
					class="radio radio-sm"
					name="collision-action"
					value="recycle_existing_keep_incoming"
					bind:group={collisionAction}
					disabled={!recycleAvailable}
				/><span
					><strong>Recycle existing; keep incoming</strong><small
						>{recycleAvailable
							? 'Moves the existing file to the configured recycle area.'
							: 'Unavailable until a recycle directory is configured.'}</small
					></span
				></label
			>
			<label class="management-selection-card"
				><input
					type="radio"
					class="radio radio-sm"
					name="collision-action"
					value="recycle_incoming_keep_existing"
					bind:group={collisionAction}
					disabled={!recycleAvailable}
				/><span
					><strong>Recycle incoming; keep existing</strong><small
						>{recycleAvailable
							? 'Moves the incoming file to the configured recycle area.'
							: 'Unavailable until a recycle directory is configured.'}</small
					></span
				></label
			>
		</fieldset>
		{#if collisionAction === 'keep_incoming_alternate'}<label class="mt-3 grid gap-1 text-sm"
				><span>Alternate relative path</span><input
					class="input input-bordered bg-base-100 font-mono"
					bind:value={alternateRelativePath}
					placeholder="Artist/Album/02 Track (alternate).flac"
				/><small class="text-base-content/45"
					>Relative to the planned destination root; absolute paths are rejected.</small
				></label
			>{/if}
		{#if collisionError}<div class="alert alert-error mt-3 text-sm" role="alert">
				{collisionError}
			</div>{/if}
		<div class="modal-action">
			<button
				class="btn btn-ghost"
				disabled={createResolution.isPending}
				onclick={() => collisionDialog.close()}>Cancel</button
			><button
				class="btn management-btn"
				disabled={!collisionRequestReady || createResolution.isPending}
				onclick={() => void resolveCollision()}
				>{#if createResolution.isPending}<span class="loading loading-spinner loading-sm"
					></span>{/if}<HardDrive class="h-4 w-4" /> Generate resolution preview</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel collision resolution" disabled={createResolution.isPending}
			>close</button
		>
	</form>
</dialog>
