<script lang="ts">
	import {
		BadgeCheck,
		ChevronRight,
		CircleAlert,
		CirclePause,
		CirclePlay,
		Database,
		Disc3,
		Download,
		FileCheck,
		FingerprintPattern,
		Info,
		ListMusic,
		OctagonX,
		RefreshCw,
		ShieldCheck,
		Sparkles,
		TriangleAlert,
		X
	} from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import MusicBrainzEditionFinder from './MusicBrainzEditionFinder.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/client';
	import type { LibraryAlbumDetail } from '$lib/types';
	import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
	import { getLibraryOperationQuery } from '$lib/queries/library/LibraryOperationQueries.svelte';
	import { controlLibraryOperation } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import {
		reidentifyLibraryAlbum,
		reenableAlbumManagement,
		selectReidentificationCandidate
	} from '$lib/queries/library/LibraryCatalogMutations.svelte';
	import {
		cancelEditionConversion,
		createEditionConversionPreflight,
		createEditionConversionPreview,
		getEditionConversionQuery,
		recheckEditionConversion,
		retryEditionConversion,
		startEditionConversion
	} from '$lib/queries/library/EditionConversionQueries.svelte';
	import { rememberLibraryManagementPreviewToken } from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		album: LibraryAlbumDetail;
		className?: string;
		autoOpen?: boolean;
		showTrigger?: boolean;
		attentionLabel?: string | null;
		onclose?: () => void;
	}
	type Candidate = OperationResponse['reidentification_candidates'][number];
	let {
		album,
		className = 'btn btn-outline btn-sm gap-2',
		autoOpen = false,
		showTrigger = true,
		attentionLabel = null,
		onclose
	}: Props = $props();
	let dialog: HTMLDialogElement;
	let confirmationDialog: HTMLDialogElement;
	let dialogHeading: HTMLHeadingElement;
	let confirmationHeading: HTMLHeadingElement;
	let opener: HTMLButtonElement | null = null;
	let confirmationOpener: HTMLButtonElement | null = null;
	let confirmationCandidate = $state<Candidate | null>(null);
	let selectedCandidateKey = $state<string | null>(null);
	let jobId = $state<string | null>(null);
	let conversionJobId = $state<string | null>(null);
	let conversionPreflightToken = $state<string | null>(null);
	let openedAutomatically = $state(false);
	const storageKey = $derived(
		`droppedneedle:album-identification:${authStore.user?.id ?? 'anonymous'}:${album.id}`
	);
	const conversionStorageKey = $derived(
		conversionJobId
			? `droppedneedle:edition-conversion-preflight:${authStore.user?.id ?? 'anonymous'}:${conversionJobId}`
			: null
	);
	const attentionDescriptionId = $derived(`reidentify-attention-${album.id}`);
	const operation = getLibraryOperationQuery(() => jobId);
	const start = reidentifyLibraryAlbum();
	const selectCandidate = selectReidentificationCandidate();
	const reenableManagement = reenableAlbumManagement();
	const conversionPreflight = createEditionConversionPreflight();
	const conversionStart = startEditionConversion();
	const conversionPreview = createEditionConversionPreview();
	const conversionRetry = retryEditionConversion();
	const conversionRecheck = recheckEditionConversion();
	const conversionCancel = cancelEditionConversion();
	const conversionQuery = getEditionConversionQuery(
		() => authStore.user?.id,
		() => conversionJobId
	);
	const conversionCandidate = $derived(conversionQuery.data ?? conversionPreflight.data ?? null);
	const conversion = $derived(
		conversionCandidate &&
			['preflight', 'acquiring', 'ready', 'needs_recheck'].includes(conversionCandidate.state)
			? conversionCandidate
			: null
	);
	const pause = controlLibraryOperation('pause');
	const resume = controlLibraryOperation('resume');
	const stop = controlLibraryOperation('stop');
	const candidates = $derived(operation.data?.reidentification_candidates ?? []);
	const selectedCandidate = $derived(
		candidates.find((candidate) => candidate.candidate_key === selectedCandidateKey) ??
			candidates[0] ??
			null
	);
	const acceptedCandidate = $derived(
		candidates.find(
			(candidate) =>
				candidate.candidate_key === operation.data?.selected_reidentification_candidate_key
		) ?? null
	);

	$effect(() => {
		if (typeof sessionStorage === 'undefined') return;
		jobId = sessionStorage.getItem(storageKey);
	});

	$effect(() => {
		if (!conversionJobId && album.active_edition_conversion?.job_id) {
			conversionJobId = album.active_edition_conversion.job_id;
		}
	});

	$effect(() => {
		if (typeof sessionStorage === 'undefined' || !conversionStorageKey || conversionPreflightToken)
			return;
		conversionPreflightToken = sessionStorage.getItem(conversionStorageKey);
	});

	$effect(() => {
		if (!autoOpen || openedAutomatically || !dialog) return;
		openedAutomatically = true;
		dialog.showModal();
		dialogHeading.focus();
	});

	function open(event: MouseEvent & { currentTarget: HTMLButtonElement }): void {
		opener = event.currentTarget;
		dialog.showModal();
		dialogHeading.focus();
	}

	function handleClose(): void {
		opener?.focus();
		onclose?.();
	}

	function forgetJob(): void {
		jobId = null;
		try {
			sessionStorage.removeItem(storageKey);
		} catch {
			// The next server-created job remains authoritative if browser storage is unavailable.
		}
	}

	async function begin(releaseMbid: string | null = null): Promise<void> {
		let job: OperationResponse;
		try {
			job = await start.mutateAsync({
				albumId: album.id,
				expectedAlbumRevision: album.row_revision,
				expectedInputRevision: album.input_revision,
				oneOffLocalMetadata: album.identification_status === 'local_metadata',
				releaseMbid
			});
		} catch {
			return;
		}
		jobId = job.id;
		try {
			sessionStorage.setItem(storageKey, job.id);
		} catch {
			// The server job remains durable and is also reachable from Library operations.
		}
	}

	async function checkAgain(): Promise<void> {
		forgetJob();
		await begin();
	}

	async function checkFoundEdition(releaseMbid: string): Promise<void> {
		await begin(releaseMbid);
	}

	function evidenceLabel(classification: string): string {
		if (classification === 'supported') return 'Supported';
		if (classification === 'contradictory') return 'Conflicts';
		return 'Unknown';
	}

	function countEvidence(candidate: Candidate, classification: string): number {
		return candidate.evidence.track_evidence.filter(
			(item) => item.classification === classification
		).length;
	}

	function hasCompleteTrackMap(candidate: Candidate): boolean {
		return (
			candidate.evidence.track_evidence.length === album.track_count &&
			countEvidence(candidate, 'supported') === album.track_count
		);
	}

	function canSealCustomEdition(candidate: Candidate): boolean {
		return (
			candidate.evidence.album_title_classification !== 'contradictory' &&
			candidate.evidence.album_artist_classification !== 'contradictory'
		);
	}

	function evidenceTone(classification: string): 'success' | 'warning' | 'neutral' {
		if (classification === 'supported') return 'success';
		if (classification === 'contradictory') return 'warning';
		return 'neutral';
	}

	function trackEvidenceTone(candidate: Candidate): 'success' | 'warning' | 'neutral' {
		if (hasCompleteTrackMap(candidate)) return 'success';
		if (countEvidence(candidate, 'contradictory')) return 'warning';
		return 'neutral';
	}

	function releaseSummary(candidate: Candidate): string {
		return [candidate.evidence.release_type, candidate.evidence.release_date?.slice(0, 4)]
			.filter(Boolean)
			.join(' · ');
	}

	function reasonLabel(reasonCode: string): string {
		const labels: Record<string, string> = {
			CONTRADICTORY: 'The local evidence conflicts with this release',
			MANUAL_EXACT_RELEASE_REQUEST: 'This exact edition was supplied by an administrator',
			RELEASE_TYPE_REQUIRES_CONFIRMATION: 'Compilation or live edition needs confirmation',
			UNSAFE_RELEASE_TYPE: 'Compilation or live edition needs confirmation',
			MULTIPLE_LIKELY_RELEASES: 'More than one release is equally likely',
			UNKNOWN_EXTRAS: 'Some local tracks cannot be matched safely',
			INCOMPLETE_SUPPORT: 'The available evidence does not support the whole album'
		};
		return labels[reasonCode] ?? reasonCode.replaceAll('_', ' ').toLowerCase();
	}

	function reviewReason(candidate: Candidate): string {
		const conflictingTracks = countEvidence(candidate, 'contradictory');
		const unknownTracks = countEvidence(candidate, 'unknown');
		const missingTracks = candidate.evidence.unmatched_expected_tracks.length;
		if (conflictingTracks) {
			return `${conflictingTracks} local ${conflictingTracks === 1 ? 'track conflicts' : 'tracks conflict'} with this edition`;
		}
		if (unknownTracks || missingTracks) {
			return 'The complete local track list cannot be verified against this edition';
		}
		const conflictingGates = [
			candidate.evidence.album_title_classification === 'contradictory' ? 'album title' : null,
			candidate.evidence.album_artist_classification === 'contradictory' ? 'album artist' : null
		].filter((label): label is string => Boolean(label));
		if (conflictingGates.length) {
			return `The local ${conflictingGates.join(' and ')} ${conflictingGates.length === 1 ? 'does' : 'do'} not match this edition`;
		}
		if (
			candidate.evidence.album_title_classification === 'unknown' ||
			candidate.evidence.album_artist_classification === 'unknown'
		) {
			return 'The local album text is not strong enough to verify this edition';
		}
		return reasonLabel(candidate.evidence.reason_code);
	}

	async function applyCandidate(
		candidate: Candidate,
		confirmation: boolean,
		decisionMode: 'exact_release' | 'custom_edition' | 'leave_unmanaged' = 'exact_release'
	): Promise<void> {
		const job = operation.data;
		if (!job) return;
		await selectCandidate.mutateAsync({
			jobId: job.id,
			expectedRevision: job.row_revision,
			candidateKey: candidate.candidate_key,
			confirmation,
			decisionMode
		});
	}

	async function createCustomEdition(candidate: Candidate): Promise<void> {
		await applyCandidate(candidate, true, 'custom_edition');
	}

	async function leaveUnmanaged(candidate: Candidate): Promise<void> {
		await applyCandidate(candidate, true, 'leave_unmanaged');
	}

	async function leaveUnmanagedWithoutCandidate(): Promise<void> {
		const job = operation.data;
		if (!job) return;
		await selectCandidate.mutateAsync({
			jobId: job.id,
			expectedRevision: job.row_revision,
			candidateKey: '',
			confirmation: true,
			decisionMode: 'leave_unmanaged'
		});
		dialog.close();
	}

	async function prepareEditionConversion(candidate: Candidate): Promise<void> {
		if (!candidate.evidence.release_mbid) return;
		const result = await conversionPreflight.mutateAsync({
			albumId: album.id,
			releaseGroupMbid: candidate.evidence.release_group_mbid,
			releaseMbid: candidate.evidence.release_mbid
		});
		conversionJobId = result.job_id;
		conversionPreflightToken = result.preflight_token;
		if (result.preflight_token) {
			try {
				sessionStorage.setItem(
					`droppedneedle:edition-conversion-preflight:${authStore.user?.id ?? 'anonymous'}:${result.job_id}`,
					result.preflight_token
				);
			} catch {
				// The server-side preflight remains durable if browser storage is unavailable.
			}
		}
	}

	async function confirmEditionConversion(): Promise<void> {
		if (!conversion || !conversionPreflightToken) return;
		await conversionStart.mutateAsync({
			jobId: conversion.job_id,
			preflightToken: conversionPreflightToken,
			expectedRevision: conversion.row_revision
		});
		if (conversionStorageKey) {
			try {
				sessionStorage.removeItem(conversionStorageKey);
			} catch {
				// A consumed token is harmless if browser storage cannot be updated.
			}
		}
		conversionPreflightToken = null;
		await conversionQuery.refetch();
	}

	async function cancelConversion(): Promise<void> {
		if (!conversion) return;
		await conversionCancel.mutateAsync({
			jobId: conversion.job_id,
			expectedRevision: conversion.row_revision
		});
		if (conversionStorageKey) {
			try {
				sessionStorage.removeItem(conversionStorageKey);
			} catch {
				// The cancelled server job remains authoritative.
			}
		}
		conversionPreflightToken = null;
		await conversionQuery.refetch();
		conversionJobId = null;
		conversionPreflight.reset();
	}

	async function openFinalPreview(): Promise<void> {
		if (!conversion) return;
		const result = await conversionPreview.mutateAsync({
			jobId: conversion.job_id,
			expectedRevision: conversion.row_revision
		});
		if (!result.status.final_preview_job_id) return;
		rememberLibraryManagementPreviewToken(result.status.final_preview_job_id, result.preview_token);
		window.location.assign(
			withBasePath(
				`/library/management/previews/${encodeURIComponent(result.status.final_preview_job_id)}`
			)
		);
	}

	function candidateErrorMessage(): string {
		const error = selectCandidate.error;
		if (!(error instanceof ApiError)) return 'Could not save this album decision.';
		return error.code === 'STALE_REVISION'
			? 'The album changed. Review the current evidence and try again.'
			: error.message;
	}

	function chooseCandidate(
		candidate: Candidate,
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): void {
		if (candidate.automatic_safe) {
			void applyCandidate(candidate, false).catch(() => undefined);
			return;
		}
		confirmationOpener = event.currentTarget;
		confirmationCandidate = candidate;
		selectCandidate.reset();
		confirmationDialog.showModal();
		confirmationHeading.focus();
	}

	async function confirmCandidate(): Promise<void> {
		if (!confirmationCandidate) return;
		try {
			await applyCandidate(confirmationCandidate, true);
		} catch {
			return;
		}
		confirmationDialog.close();
		confirmationCandidate = null;
	}
</script>

{#if showTrigger}
	<div
		class="identification-trigger"
		class:identification-trigger--attention={Boolean(attentionLabel)}
	>
		<button
			class={attentionLabel ? `${className} identification-trigger-warning` : className}
			aria-describedby={attentionLabel ? attentionDescriptionId : undefined}
			onclick={open}
		>
			{#if attentionLabel}<TriangleAlert class="h-4 w-4" />{:else}<RefreshCw class="h-4 w-4" />{/if}
			Re-identify…
		</button>
		{#if attentionLabel}<span id={attentionDescriptionId}>{attentionLabel}</span>{/if}
	</div>
{/if}

<dialog
	bind:this={dialog}
	class="modal identification-dialog"
	aria-labelledby="identification-panel-title"
	onclose={handleClose}
>
	<div class="modal-box identification-workspace" data-testid="identification-workspace">
		<header class="identification-modal-header">
			<div class="identification-modal-mark" aria-hidden="true">
				<FingerprintPattern class="h-6 w-6" />
			</div>
			<div class="min-w-0 flex-1">
				<p class="identification-kicker">Read-only identity desk</p>
				<h2
					bind:this={dialogHeading}
					id="identification-panel-title"
					tabindex="-1"
					class="hero-title truncate text-2xl font-bold sm:text-3xl"
				>
					Identify {album.title}
				</h2>
				<p class="mt-1 max-w-2xl text-sm text-base-content/60">
					Compare this local release with exact MusicBrainz editions before attaching an identity.
				</p>
			</div>
			<form method="dialog">
				<button class="btn btn-ghost btn-sm btn-circle" aria-label="Close">
					<X class="h-5 w-5" />
				</button>
			</form>
		</header>

		<div class="identification-scroll-region" data-testid="identification-scroll-region">
			{#if album.management_identity_kind === 'custom_edition'}
				<section class="identification-current-edition" aria-label="Current Custom edition">
					<div class="identification-current-edition-mark"><Sparkles class="h-5 w-5" /></div>
					<div class="min-w-0 flex-1">
						<p class="identification-kicker">Currently attached · Custom edition</p>
						<h3 class="hero-title text-lg font-bold">
							File organization uses your local track list
						</h3>
						<p>
							{album.custom_manifest_recognized_track_count} of {album.custom_manifest_track_count}
							tracks recognized; {Math.max(
								0,
								album.custom_manifest_track_count - album.custom_manifest_recognized_track_count
							)} keep their local identity.
						</p>
					</div>
					<span class:badge-warning={album.custom_manifest_stale} class="badge badge-sm">
						{album.custom_manifest_stale
							? 'Review and reseal required'
							: `Manifest v${album.custom_manifest_version ?? 1}`}
					</span>
				</section>
			{/if}

			{#if album.management_excluded}
				<section class="identification-current-edition" aria-label="File organization excluded">
					<div class="identification-current-edition-mark"><CirclePause class="h-5 w-5" /></div>
					<div class="min-w-0 flex-1">
						<p class="identification-kicker">Left unmanaged</p>
						<h3 class="hero-title text-lg font-bold">Management warnings are paused</h3>
						<p>Valid MusicBrainz links remain attached. Files and tags are unchanged.</p>
					</div>
					<button
						class="btn btn-outline btn-sm"
						disabled={reenableManagement.isPending || album.management_exclusion_revision === null}
						onclick={() =>
							album.management_exclusion_revision !== null &&
							void reenableManagement.mutateAsync({
								albumId: album.id,
								expectedRevision: album.management_exclusion_revision
							})}>Re-enable management</button
					>
				</section>
			{/if}

			{#if conversion}
				<section class="identification-conversion" aria-labelledby="edition-conversion-title">
					<header>
						<div>
							<p class="identification-kicker">Match exact edition</p>
							<h3 id="edition-conversion-title" class="hero-title text-xl font-bold">
								{conversion.album_title}
							</h3>
							<p>{conversion.artist_name}</p>
						</div>
						<span class="badge badge-outline">{conversion.state.replaceAll('_', ' ')}</span>
					</header>
					<div class="identification-conversion-counts">
						<span><strong>{conversion.kept_count}</strong> keep</span>
						<span><strong>{conversion.acquire_count}</strong> acquire</span>
						<span><strong>{conversion.recycle_count}</strong> recycle</span>
					</div>
					{#if conversion.state === 'preflight'}
						<p class="identification-conversion-note">
							No library files change while the missing recordings are acquired and verified.
						</p>
						{#if !conversion.download_source_ready}
							<p class="identification-policy-note" data-tone="warning">
								<CircleAlert class="h-4 w-4 shrink-0" /> Set up any music acquisition source before starting
								this conversion.
							</p>
						{/if}
						<button
							class="btn btn-primary btn-sm gap-2"
							disabled={!conversion.download_source_ready ||
								!conversionPreflightToken ||
								conversionStart.isPending}
							onclick={() => void confirmEditionConversion()}
						>
							<Download class="h-4 w-4" /> Confirm and acquire missing tracks
						</button>
					{:else if conversion.state === 'acquiring'}
						<progress
							class="progress progress-primary w-full"
							value={conversion.staged_count}
							max={Math.max(1, conversion.acquire_count)}
							aria-label="Edition acquisition progress"
						></progress>
						<p class="identification-conversion-note">
							{conversion.staged_count} of {conversion.acquire_count} acquired and verified.
						</p>
						{#if conversion.failed_count}
							<button
								class="btn btn-outline btn-sm"
								disabled={conversionRetry.isPending}
								onclick={() =>
									void conversionRetry
										.mutateAsync({
											jobId: conversion.job_id,
											targetOrdinals: conversion.targets
												.filter((target) => target.state === 'failed')
												.map((target) => target.ordinal),
											expectedRevision: conversion.row_revision
										})
										.then(() => conversionQuery.refetch())}
								>Retry {conversion.failed_count} unresolved</button
							>
						{/if}
					{:else if conversion.state === 'needs_recheck'}
						<p class="identification-policy-note" data-tone="warning">
							<CircleAlert class="h-4 w-4 shrink-0" /> The album or its provider identity changed. Recheck
							before continuing.
						</p>
						<button
							class="btn btn-primary btn-sm gap-2"
							disabled={conversionRecheck.isPending}
							onclick={() =>
								void conversionRecheck
									.mutateAsync({
										jobId: conversion.job_id,
										expectedRevision: conversion.row_revision
									})
									.then(() => conversionQuery.refetch())}
							><RefreshCw class="h-4 w-4" /> Recheck album</button
						>
					{:else if conversion.state === 'ready'}
						<p class="identification-conversion-note">
							Every exact-release track has one verified source. Review the single sealed file plan
							before anything changes.
						</p>
						<button
							class="btn btn-primary btn-sm gap-2"
							disabled={conversionPreview.isPending}
							onclick={() => void openFinalPreview()}
						>
							<FileCheck class="h-4 w-4" /> Review final changes
						</button>
					{/if}
					{#if ['preflight', 'acquiring', 'ready', 'needs_recheck'].includes(conversion.state)}
						<button
							class="btn btn-ghost btn-sm text-error"
							disabled={conversionCancel.isPending}
							onclick={() => void cancelConversion().catch(() => undefined)}
							>Cancel conversion</button
						>
					{/if}
				</section>
			{/if}

			{#if album.identification_status === 'local_metadata' && !operation.data}
				<div class="identification-policy-note">
					<Info class="h-4 w-4 shrink-0" />
					<p>
						This is a one-off identification check. The Local metadata policy will still apply to
						future scans.
					</p>
				</div>
			{/if}

			{#if operation.isError}
				<div class="identification-empty-state">
					<CircleAlert class="h-9 w-9 text-error" />
					<div>
						<h3 class="font-semibold">The saved check could not be loaded</h3>
						<p class="mt-1 text-sm text-base-content/60">
							The earlier job remains untouched. Start a fresh evidence check when you are ready.
						</p>
					</div>
					<button class="btn btn-primary btn-sm" onclick={forgetJob}>Start a new check</button>
				</div>
			{:else if !operation.data}
				<div class="identification-launchpad">
					<div class="identification-empty-state identification-empty-state--ready">
						<div class="identification-empty-illustration" aria-hidden="true">
							<Disc3 class="h-10 w-10" />
							<FingerprintPattern class="h-5 w-5" />
						</div>
						<div class="max-w-xl">
							<p class="identification-kicker">
								{album.musicbrainz_release_id ? 'Attached identity' : 'Evidence check'}
							</p>
							<h3 class="hero-title mt-1 text-2xl font-bold">
								{album.musicbrainz_release_id
									? 'Check or replace the exact edition'
									: 'Find the exact edition'}
							</h3>
							<p class="mt-2 text-sm leading-6 text-base-content/60">
								DroppedNeedle compares album, artist, and per-track evidence. The job continues on
								the server if you close this dialog.
							</p>
						</div>
						<div class="identification-start-actions">
							<button
								class="btn btn-primary gap-2"
								disabled={start.isPending}
								onclick={() => void begin(album.musicbrainz_release_id ?? null)}
							>
								{#if start.isPending}<span class="loading loading-spinner loading-sm"
									></span>{:else}<FingerprintPattern class="h-4 w-4" />{/if}
								{album.musicbrainz_release_id ? 'Check attached identity' : 'Start identification'}
							</button>
						</div>
					</div>
					<MusicBrainzEditionFinder
						albumId={album.id}
						artistName={album.artist_name}
						albumTitle={album.title}
						currentReleaseMbid={album.musicbrainz_release_id}
						mappedTrackCount={album.mapped_track_count}
						totalTrackCount={album.track_count}
						oncheck={checkFoundEdition}
						checking={start.isPending}
					/>
					{#if start.isError}
						<p class="text-sm text-error">Could not start identification. Try again.</p>
					{/if}
				</div>
			{:else}
				{@const job = operation.data}
				<section class="identification-job-strip" aria-label="Identification job status">
					<div class="identification-job-state" data-state={job.state}>
						<span class="identification-job-pulse" aria-hidden="true"></span>
						<div>
							<strong
								>{job.state === 'ready' ? 'Evidence ready' : job.state.replaceAll('_', ' ')}</strong
							>
							<p>
								{job.state === 'ready'
									? `${job.reidentification_candidates.length} release ${job.reidentification_candidates.length === 1 ? 'candidate' : 'candidates'}`
									: job.terminal_code
										? job.terminal_code.replaceAll('_', ' ').toLowerCase()
										: 'Checking local evidence'}
							</p>
						</div>
					</div>
					<div class="flex flex-wrap items-center justify-end gap-1">
						{#if job.state === 'ready'}
							<button
								class="btn btn-ghost btn-sm gap-1"
								disabled={start.isPending}
								onclick={() => void checkAgain()}
							>
								<RefreshCw class="h-4 w-4 {start.isPending ? 'animate-spin' : ''}" /> Check again
							</button>
							<button
								class="btn btn-ghost btn-sm gap-1 text-error"
								onclick={() =>
									void stop
										.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
										.catch(() => undefined)}
								aria-label="Discard identification evidence"
							>
								<OctagonX class="h-4 w-4" /> Discard
							</button>
						{/if}
						{#if job.state === 'running'}
							<button
								class="btn btn-ghost btn-sm"
								onclick={() =>
									void pause
										.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
										.catch(() => undefined)}
								aria-label="Pause identification"><CirclePause class="h-4 w-4" /> Pause</button
							>
						{:else if job.state === 'paused'}
							<button
								class="btn btn-ghost btn-sm"
								onclick={() =>
									void resume
										.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
										.catch(() => undefined)}
								aria-label="Resume identification"><CirclePlay class="h-4 w-4" /> Resume</button
							>
						{/if}
						{#if ['queued', 'running', 'paused'].includes(job.state)}
							<button
								class="btn btn-ghost btn-sm text-error"
								onclick={() =>
									void stop
										.mutateAsync({ jobId: job.id, expectedRevision: job.row_revision })
										.catch(() => undefined)}
								aria-label="Stop identification"><OctagonX class="h-4 w-4" /> Stop</button
							>
						{/if}
					</div>
					{#if job.state !== 'ready'}
						<progress
							class="progress progress-primary col-span-full w-full"
							value={job.completed_count}
							max={Math.max(1, job.expected_work_count)}
							aria-label="Identification progress"
						></progress>
					{/if}
				</section>

				{#if job.state === 'succeeded' && job.terminal_code === 'IDENTIFIED'}
					<section class="identification-success-receipt" aria-labelledby="identity-saved-title">
						<div class="identification-success-mark" aria-hidden="true">
							<BadgeCheck class="h-9 w-9" />
						</div>
						<div class="identification-success-copy">
							<p class="identification-kicker">Manual identity saved</p>
							<h3 id="identity-saved-title" class="hero-title mt-1 text-2xl font-bold">
								Identity attached
							</h3>
							<p>
								This exact edition and its per-track map are now DroppedNeedle's durable catalog
								identity. Future scans preserve the decision until an administrator resets it.
							</p>
						</div>

						{#if acceptedCandidate}
							<article class="identification-accepted-release">
								<div class="identification-accepted-art">
									<AlbumImage
										mbid={acceptedCandidate.evidence.release_group_mbid}
										alt={`Cover for ${acceptedCandidate.evidence.album_title}`}
										size="full"
										requestSize={250}
										rounded="xl"
										className="h-full w-full"
										retryOnError={false}
									/>
								</div>
								<div class="min-w-0">
									<p class="identification-kicker">Accepted edition</p>
									<h4 class="hero-title mt-1 text-xl font-bold">
										{acceptedCandidate.evidence.album_title}
									</h4>
									<p class="mt-1 text-sm text-base-content/60">
										{acceptedCandidate.evidence
											.album_artist_name}{#if releaseSummary(acceptedCandidate)}
											<span aria-hidden="true"> · </span>{releaseSummary(acceptedCandidate)}{/if}
									</p>
								</div>
							</article>
						{/if}

						<div class="identification-success-contract">
							<div>
								<Database class="h-5 w-5" />
								<span><strong>Catalog identity</strong>Saved</span>
							</div>
							<div>
								<FileCheck class="h-5 w-5" />
								<span
									><strong>Track mappings</strong>{acceptedCandidate
										? `${countEvidence(acceptedCandidate, 'supported')} exact`
										: 'Saved'}</span
								>
							</div>
							<div>
								<ShieldCheck class="h-5 w-5" />
								<span><strong>Music files</strong>Unchanged</span>
							</div>
						</div>
					</section>
				{:else if job.reidentification_candidates.length && selectedCandidate}
					<section
						class="identification-workbench"
						aria-labelledby="identification-candidates-title"
					>
						<nav class="identification-candidate-rail" aria-label="Release candidates">
							<div class="identification-rail-heading">
								<div>
									<p class="identification-kicker">Candidate set</p>
									<h3 id="identification-candidates-title" class="hero-title text-lg font-bold">
										Release candidates
									</h3>
								</div>
								<span class="identification-count">{job.reidentification_candidates.length}</span>
							</div>
							<div class="identification-candidate-list">
								{#each job.reidentification_candidates as candidate, index (candidate.candidate_key)}
									<button
										class="identification-candidate"
										data-selected={candidate.candidate_key === selectedCandidate.candidate_key}
										aria-pressed={candidate.candidate_key === selectedCandidate.candidate_key}
										onclick={() => (selectedCandidateKey = candidate.candidate_key)}
									>
										<span class="identification-candidate-rank"
											>{String(index + 1).padStart(2, '0')}</span
										>
										<span class="min-w-0 flex-1">
											<strong>{candidate.evidence.album_title}</strong>
											<small>{candidate.evidence.album_artist_name}</small>
											<span class="identification-candidate-signals">
												<span data-tone={trackEvidenceTone(candidate)}
													>{countEvidence(candidate, 'supported')} matched</span
												>
												{#if countEvidence(candidate, 'contradictory')}<span data-tone="warning"
														>{countEvidence(candidate, 'contradictory')} conflicts</span
													>{/if}
												{#if candidate.evidence.unmatched_expected_tracks.length}<span
														>{candidate.evidence.unmatched_expected_tracks.length} missing</span
													>{/if}
											</span>
										</span>
										<span class="identification-score">{candidate.evidence.score.toFixed(2)}</span>
										<ChevronRight class="h-4 w-4 shrink-0" aria-hidden="true" />
									</button>
								{/each}
							</div>
						</nav>

						<article
							class="identification-evidence-dossier"
							aria-live="polite"
							data-testid="identification-evidence-dossier"
						>
							<header class="identification-release-header">
								<div class="identification-release-art">
									<AlbumImage
										mbid={selectedCandidate.evidence.release_group_mbid}
										alt={`Cover for ${selectedCandidate.evidence.album_title}`}
										size="full"
										requestSize={250}
										rounded="xl"
										className="h-full w-full"
										retryOnError={false}
									/>
								</div>
								<div class="min-w-0 flex-1">
									<p class="identification-kicker">Selected edition</p>
									<h3 class="hero-title mt-1 text-2xl font-bold">
										{selectedCandidate.evidence.album_title}
									</h3>
									<p class="mt-1 text-base-content/65">
										{selectedCandidate.evidence
											.album_artist_name}{#if releaseSummary(selectedCandidate)}
											<span aria-hidden="true"> · </span>{releaseSummary(selectedCandidate)}{/if}
									</p>
									<div class="identification-release-badges">
										{#if hasCompleteTrackMap(selectedCandidate)}
											<span data-tone="success"
												><BadgeCheck class="h-3.5 w-3.5" /> Complete track map</span
											>
										{/if}
										<span data-tone={selectedCandidate.automatic_safe ? 'success' : 'warning'}>
											{selectedCandidate.automatic_safe
												? 'Strong evidence'
												: 'Administrator review'}
										</span>
									</div>
								</div>
								<div class="identification-score-card">
									<strong>{selectedCandidate.evidence.score.toFixed(2)}</strong>
									<span>evidence score</span>
								</div>
							</header>

							<div class="identification-dossier-body">
								<section aria-labelledby="evidence-gates-title">
									<div class="identification-section-heading">
										<div>
											<p class="identification-kicker">Decision evidence</p>
											<h4 id="evidence-gates-title" class="hero-title font-bold">Release fit</h4>
										</div>
										<span class="text-xs text-base-content/45">Local → selected edition</span>
									</div>
									<div class="identification-gate-grid">
										<div
											data-tone={evidenceTone(
												selectedCandidate.evidence.album_title_classification
											)}
										>
											<span>Album title</span>
											<strong
												>{evidenceLabel(
													selectedCandidate.evidence.album_title_classification
												)}</strong
											>
										</div>
										<div
											data-tone={evidenceTone(
												selectedCandidate.evidence.album_artist_classification
											)}
										>
											<span>Album artist</span>
											<strong
												>{evidenceLabel(
													selectedCandidate.evidence.album_artist_classification
												)}</strong
											>
										</div>
										<div data-tone={trackEvidenceTone(selectedCandidate)}>
											<span>Track map</span>
											<strong
												>{countEvidence(selectedCandidate, 'supported')} of {selectedCandidate
													.evidence.track_evidence.length} matched</strong
											>
										</div>
									</div>
								</section>

								{#if hasCompleteTrackMap(selectedCandidate) && !selectedCandidate.automatic_safe}
									<div class="identification-insight" data-tone="success">
										<FileCheck class="h-5 w-5 shrink-0" />
										<p>
											<strong>Every local track maps to this exact edition.</strong> The text currently
											attached to the album does not agree, so an administrator must confirm the catalog
											identity.
										</p>
									</div>
								{:else if selectedCandidate.automatic_safe}
									<div class="identification-insight" data-tone="success">
										<ShieldCheck class="h-5 w-5 shrink-0" />
										<p>
											<strong>All required evidence gates pass.</strong> This identity can be attached
											directly.
										</p>
									</div>
								{:else}
									<div class="identification-insight" data-tone="warning">
										<CircleAlert class="h-5 w-5 shrink-0" />
										<p>
											<strong>Review is required.</strong>
											{reviewReason(selectedCandidate)}.
										</p>
									</div>
								{/if}

								{#if countEvidence(selectedCandidate, 'contradictory')}
									<section class="identification-track-exceptions">
										<h4><CircleAlert class="h-4 w-4" /> Conflicting tracks</h4>
										<ul>
											{#each selectedCandidate.evidence.track_evidence.filter((item) => item.classification === 'contradictory') as item (item.local_track_id)}
												<li>{item.candidate_track_title ?? item.local_track_id}</li>
											{/each}
										</ul>
									</section>
								{/if}

								{#if countEvidence(selectedCandidate, 'unknown') || selectedCandidate.evidence.unmatched_expected_tracks.length}
									<div class="identification-track-note">
										<ListMusic class="h-4 w-4 shrink-0" />
										<span>
											{countEvidence(selectedCandidate, 'unknown')} local tracks have unknown evidence;
											{selectedCandidate.evidence.unmatched_expected_tracks.length} expected release tracks
											are missing.
										</span>
									</div>
								{/if}

								<details class="identification-technical-details">
									<summary><Database class="h-4 w-4" /> Technical identity</summary>
									<div>
										<span>Release group</span>
										<code>{selectedCandidate.evidence.release_group_mbid}</code>
										{#if selectedCandidate.evidence.release_mbid}
											<span>Exact release</span>
											<code>{selectedCandidate.evidence.release_mbid}</code>
										{/if}
									</div>
								</details>
							</div>

							{#if hasCompleteTrackMap(selectedCandidate)}
								<footer class="identification-dossier-action">
									<div>
										<strong>Attach exact edition</strong>
										<span>Audio, tags, artwork, and file paths stay untouched.</span>
									</div>
									<button
										class="btn btn-primary gap-2"
										disabled={job.state !== 'ready' || selectCandidate.isPending}
										onclick={(event) => chooseCandidate(selectedCandidate, event)}
									>
										{selectedCandidate.automatic_safe ? 'Use this identity' : 'Review and use...'}
										<ChevronRight class="h-4 w-4" />
									</button>
								</footer>
							{:else}
								<section class="identification-choice-grid" aria-labelledby="album-outcome-title">
									<div class="identification-choice-heading">
										<p class="identification-kicker">Choose what to do next</p>
										<h4 id="album-outcome-title" class="hero-title text-xl font-bold">
											This album does not match one exact edition
										</h4>
									</div>
									<article>
										<div class="identification-choice-icon"><Sparkles class="h-5 w-5" /></div>
										<h5>Manage my current album</h5>
										<p>
											Seal its current titles and order as a Custom edition. {countEvidence(
												selectedCandidate,
												'supported'
											)} of {album.track_count} tracks are recognized; {Math.max(
												0,
												album.track_count - countEvidence(selectedCandidate, 'supported')
											)} keep their local identity.
										</p>
										<button
											class="btn btn-primary btn-sm"
											disabled={selectCandidate.isPending ||
												!canSealCustomEdition(selectedCandidate)}
											onclick={() =>
												void createCustomEdition(selectedCandidate).catch(() => undefined)}
											>Create Custom edition</button
										>
										{#if !canSealCustomEdition(selectedCandidate)}
											<small>This candidate conflicts with the album title or artist.</small>
										{/if}
									</article>
									<article>
										<div class="identification-choice-icon"><Download class="h-5 w-5" /></div>
										<h5>Match this edition's track list</h5>
										<p>
											Keep verified local audio, acquire {selectedCandidate.evidence
												.unmatched_expected_tracks.length} missing tracks, and recycle conflicts only
											after one final preview.
										</p>
										<button
											class="btn btn-outline btn-sm"
											disabled={!selectedCandidate.evidence.release_mbid ||
												conversionPreflight.isPending ||
												Boolean(conversion)}
											onclick={() =>
												void prepareEditionConversion(selectedCandidate).catch(() => undefined)}
											>Prepare conversion</button
										>
									</article>
									<article>
										<div class="identification-choice-icon"><CirclePause class="h-5 w-5" /></div>
										<h5>Leave unmanaged</h5>
										<p>
											Keep valid release-group and recording links, remove the unsupported
											exact-edition claim, and pause management warnings.
										</p>
										<button
											class="btn btn-ghost btn-sm"
											disabled={selectCandidate.isPending}
											onclick={() => void leaveUnmanaged(selectedCandidate).catch(() => undefined)}
											>Leave unmanaged</button
										>
									</article>
								</section>
							{/if}
						</article>
					</section>
					<div class="identification-find-another">
						<MusicBrainzEditionFinder
							albumId={album.id}
							artistName={album.artist_name}
							albumTitle={album.title}
							currentReleaseMbid={album.musicbrainz_release_id}
							mappedTrackCount={album.mapped_track_count}
							totalTrackCount={album.track_count}
							oncheck={checkFoundEdition}
							checking={start.isPending}
						/>
					</div>
				{:else if job.state === 'ready' || job.state === 'succeeded'}
					<div class="identification-empty-state">
						<CircleAlert class="h-9 w-9 text-warning" />
						<div>
							<h3 class="font-semibold">
								{job.terminal_code === 'NO_EXTERNAL_RESULT'
									? 'No release candidates were found'
									: 'Identity evidence needs review'}
							</h3>
							<p class="mt-1 text-sm text-base-content/60">
								{job.terminal_code === 'NO_EXTERNAL_RESULT'
									? 'Check the local album metadata, or verify a known exact edition below.'
									: 'The stored identity evidence is incomplete or conflicting. Verify the exact edition below.'}
							</p>
						</div>
						<MusicBrainzEditionFinder
							albumId={album.id}
							artistName={album.artist_name}
							albumTitle={album.title}
							currentReleaseMbid={album.musicbrainz_release_id}
							mappedTrackCount={album.mapped_track_count}
							totalTrackCount={album.track_count}
							oncheck={checkFoundEdition}
							checking={start.isPending}
						/>
						<div class="mt-3 flex items-center gap-2">
							<button
								class="btn btn-outline btn-sm"
								disabled={selectCandidate.isPending}
								onclick={() => void leaveUnmanagedWithoutCandidate().catch(() => undefined)}
								>Leave unmanaged</button
							>
							<p class="text-xs text-base-content/50">
								Keep valid release-group and recording links and pause management warnings.
							</p>
						</div>
					</div>
				{/if}

				{#if selectCandidate.isError}
					<div class="identification-policy-note" data-tone="warning">
						<CircleAlert class="h-4 w-4 shrink-0" />
						<p>{candidateErrorMessage()}</p>
					</div>
				{/if}
			{/if}
		</div>

		<footer class="identification-modal-footer">
			<div class="identification-safety-note">
				<ShieldCheck class="h-4 w-4" />
				<span><strong>Catalog only.</strong> This screen never writes music files.</span>
			</div>
			{#if operation.data && ['succeeded', 'failed', 'cancelled', 'stopped'].includes(operation.data.state)}
				<button class="btn btn-outline btn-sm" onclick={forgetJob}>Start another check</button>
			{/if}
		</footer>
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>

<dialog
	bind:this={confirmationDialog}
	class="modal identification-dialog"
	aria-labelledby="identification-confirm-title"
	onclose={() => confirmationOpener?.focus()}
>
	<div class="modal-box identification-confirmation" data-testid="identification-confirmation">
		<header class="identification-confirmation-header">
			<div class="identification-confirmation-mark" aria-hidden="true">
				<CircleAlert class="h-5 w-5" />
			</div>
			<div class="min-w-0 flex-1">
				<p class="identification-kicker">Manual identity decision</p>
				<h2
					bind:this={confirmationHeading}
					id="identification-confirm-title"
					tabindex="-1"
					class="hero-title mt-1 text-xl font-bold"
				>
					Use this identity despite conflicting evidence?
				</h2>
			</div>
			<button
				class="btn btn-ghost btn-sm btn-circle"
				onclick={() => confirmationDialog.close()}
				aria-label="Close confirmation"><X class="h-5 w-5" /></button
			>
		</header>

		<div class="identification-confirmation-body" data-testid="identification-confirmation-body">
			{#if confirmationCandidate}
				<section class="identification-confirmation-release">
					<div class="identification-confirmation-art">
						<AlbumImage
							mbid={confirmationCandidate.evidence.release_group_mbid}
							alt={`Cover for ${confirmationCandidate.evidence.album_title}`}
							size="full"
							requestSize={250}
							rounded="xl"
							className="h-full w-full"
							retryOnError={false}
						/>
					</div>
					<div class="min-w-0">
						<p class="identification-kicker">Identity to attach</p>
						<h3 class="hero-title mt-1 text-xl font-bold">
							{confirmationCandidate.evidence.album_title}
						</h3>
						<p class="text-sm text-base-content/60">
							{confirmationCandidate.evidence.album_artist_name}
						</p>
						{#if hasCompleteTrackMap(confirmationCandidate)}
							<span class="identification-complete-map"
								><BadgeCheck class="h-3.5 w-3.5" /> All {confirmationCandidate.evidence
									.track_evidence.length} tracks mapped</span
							>
						{/if}
					</div>
				</section>

				<div class="identification-change-contract">
					<div>
						<Database class="h-5 w-5" />
						<span><strong>Changes</strong>Catalog identity and exact track mappings</span>
					</div>
					<div>
						<ShieldCheck class="h-5 w-5" />
						<span><strong>Untouched</strong>Audio, tags, artwork, folders, and filenames</span>
					</div>
				</div>

				<section class="identification-warning-card">
					<div class="identification-section-heading">
						<div>
							<p class="identification-kicker">Why confirmation is required</p>
							<h3 class="hero-title font-bold">
								{reviewReason(confirmationCandidate)}
							</h3>
						</div>
						<CircleAlert class="h-5 w-5 shrink-0 text-warning" />
					</div>
					<h4 class="mt-4 text-sm font-semibold">Failed evidence gates</h4>
					<ul class="identification-failed-gates">
						{#if confirmationCandidate.evidence.album_title_classification !== 'supported'}
							<li>
								<span>Album title</span>
								<strong
									>{evidenceLabel(
										confirmationCandidate.evidence.album_title_classification
									)}</strong
								>
							</li>
						{/if}
						{#if confirmationCandidate.evidence.album_artist_classification !== 'supported'}
							<li>
								<span>Album artist</span>
								<strong
									>{evidenceLabel(
										confirmationCandidate.evidence.album_artist_classification
									)}</strong
								>
							</li>
						{/if}
						{#if confirmationCandidate.evidence.unmatched_expected_tracks.length}
							<li>
								<span>Edition completeness</span>
								<strong
									>{confirmationCandidate.evidence.unmatched_expected_tracks.length} missing</strong
								>
							</li>
						{/if}
					</ul>
				</section>

				{#if countEvidence(confirmationCandidate, 'contradictory')}
					<section class="identification-confirmation-list">
						<h3>Contradictory local tracks</h3>
						<ul>
							{#each confirmationCandidate.evidence.track_evidence.filter((item) => item.classification === 'contradictory') as item (item.local_track_id)}
								<li>
									<span>{item.candidate_track_title ?? 'No candidate track'}</span>
									<code>{item.local_track_id}</code>
								</li>
							{/each}
						</ul>
					</section>
				{/if}

				{#if countEvidence(confirmationCandidate, 'unknown')}
					<section class="identification-confirmation-list">
						<h3>Unknown local tracks</h3>
						<ul>
							{#each confirmationCandidate.evidence.track_evidence.filter((item) => item.classification === 'unknown') as item (item.local_track_id)}
								<li><code>{item.local_track_id}</code></li>
							{/each}
						</ul>
					</section>
				{/if}

				<details
					class="identification-technical-details identification-technical-details--confirmation"
				>
					<summary><Database class="h-4 w-4" /> Technical identity record</summary>
					<div>
						<span>Release group</span>
						<code>Release group: {confirmationCandidate.evidence.release_group_mbid}</code>
						{#if confirmationCandidate.evidence.release_mbid}
							<span>Exact release</span>
							<code>Release: {confirmationCandidate.evidence.release_mbid}</code>
						{/if}
						{#if album.musicbrainz_release_group_id}
							<span>Current album identity</span>
							<code>Current album ID: {album.musicbrainz_release_group_id}</code>
						{/if}
						{#if confirmationCandidate.evidence.track_evidence.some((item) => item.classification === 'supported' && item.recording_mbid)}
							<span>Supported track mappings</span>
							<ul>
								{#each confirmationCandidate.evidence.track_evidence.filter((item) => item.classification === 'supported' && item.recording_mbid) as item (item.local_track_id)}
									<li><code>{item.local_track_id}</code> → <code>{item.recording_mbid}</code></li>
								{/each}
							</ul>
						{/if}
					</div>
				</details>

				<p class="identification-durable-note">
					<ShieldCheck class="h-4 w-4 shrink-0" />
					<span>
						This becomes a durable manual identity. Later scans preserve it until an administrator
						resets it.
					</span>
				</p>
			{/if}

			{#if selectCandidate.isError}
				<div class="identification-policy-note" data-tone="warning" role="alert">
					<CircleAlert class="h-4 w-4 shrink-0" />
					<p>{candidateErrorMessage()}</p>
				</div>
			{/if}
		</div>

		<footer class="identification-confirmation-footer">
			<button class="btn btn-ghost" onclick={() => confirmationDialog.close()}>Cancel</button>
			<button
				class="btn btn-warning gap-2"
				disabled={selectCandidate.isPending}
				onclick={() => void confirmCandidate()}
			>
				{#if selectCandidate.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
				Use conflicting identity
			</button>
		</footer>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close conflicting identity confirmation">close</button>
	</form>
</dialog>
