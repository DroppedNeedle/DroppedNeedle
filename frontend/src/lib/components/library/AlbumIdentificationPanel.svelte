<script lang="ts">
	import { CirclePause, CirclePlay, OctagonX, RefreshCw } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { LibraryAlbumDetail } from '$lib/types';
	import type { OperationResponse } from '$lib/queries/library/LibraryOperationsTypes';
	import { getLibraryOperationQuery } from '$lib/queries/library/LibraryOperationQueries.svelte';
	import { controlLibraryOperation } from '$lib/queries/library/LibraryOperationMutations.svelte';
	import {
		reidentifyLibraryAlbum,
		selectReidentificationCandidate
	} from '$lib/queries/library/LibraryCatalogMutations.svelte';

	interface Props {
		album: LibraryAlbumDetail;
	}
	type Candidate = OperationResponse['reidentification_candidates'][number];
	let { album }: Props = $props();
	let dialog: HTMLDialogElement;
	let confirmationDialog: HTMLDialogElement;
	let dialogHeading: HTMLHeadingElement;
	let confirmationHeading: HTMLHeadingElement;
	let opener: HTMLButtonElement | null = null;
	let confirmationOpener: HTMLButtonElement | null = null;
	let confirmationCandidate = $state<Candidate | null>(null);
	let jobId = $state<string | null>(null);
	const storageKey = $derived(
		`droppedneedle:album-identification:${authStore.user?.id ?? 'anonymous'}:${album.id}`
	);
	const operation = getLibraryOperationQuery(() => jobId);
	const start = reidentifyLibraryAlbum();
	const selectCandidate = selectReidentificationCandidate();
	const pause = controlLibraryOperation('pause');
	const resume = controlLibraryOperation('resume');
	const stop = controlLibraryOperation('stop');

	$effect(() => {
		if (typeof sessionStorage === 'undefined') return;
		jobId = sessionStorage.getItem(storageKey);
	});

	function open(event: MouseEvent & { currentTarget: HTMLButtonElement }): void {
		opener = event.currentTarget;
		dialog.showModal();
		dialogHeading.focus();
	}

	function forgetJob(): void {
		jobId = null;
		try {
			sessionStorage.removeItem(storageKey);
		} catch {
			// The next server-created job remains authoritative if browser storage is unavailable.
		}
	}

	async function begin(): Promise<void> {
		let job: OperationResponse;
		try {
			job = await start.mutateAsync({
				albumId: album.id,
				expectedAlbumRevision: album.row_revision,
				expectedInputRevision: album.input_revision,
				oneOffLocalMetadata: album.identification_status === 'local_metadata'
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

	function reasonLabel(reasonCode: string): string {
		const labels: Record<string, string> = {
			CONTRADICTORY: 'The local evidence conflicts with this release',
			MULTIPLE_LIKELY_RELEASES: 'More than one release is equally likely',
			UNKNOWN_EXTRAS: 'Some local tracks cannot be matched safely',
			INCOMPLETE_SUPPORT: 'The available evidence does not support the whole album'
		};
		return labels[reasonCode] ?? reasonCode.replaceAll('_', ' ').toLowerCase();
	}

	async function applyCandidate(candidate: Candidate, confirmation: boolean): Promise<void> {
		const job = operation.data;
		if (!job) return;
		await selectCandidate.mutateAsync({
			jobId: job.id,
			expectedRevision: job.row_revision,
			candidateKey: candidate.candidate_key,
			confirmation
		});
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

<button class="btn btn-outline btn-sm gap-2" onclick={open}>
	<RefreshCw class="h-4 w-4" /> Re-identify...
</button>

<dialog
	bind:this={dialog}
	class="modal"
	aria-labelledby="identification-panel-title"
	onclose={() => opener?.focus()}
>
	<div class="modal-box max-w-3xl">
		<div class="flex items-start gap-3">
			<div class="min-w-0 flex-1">
				<h2
					bind:this={dialogHeading}
					id="identification-panel-title"
					tabindex="-1"
					class="text-xl font-bold"
				>
					Identify {album.title}
				</h2>
				<p class="mt-1 text-sm text-base-content/60">
					This checks the current local album against available release evidence. Your files and
					tags will not change.
				</p>
			</div>
			<form method="dialog"><button class="btn btn-ghost btn-sm">Close</button></form>
		</div>

		{#if album.identification_status === 'local_metadata' && !operation.data}
			<div class="alert alert-warning mt-4 text-sm">
				This is a one-off identification check. The Local metadata policy will still apply to future
				scans.
			</div>
		{/if}

		{#if operation.isError}
			<div class="alert alert-error mt-5 text-sm">
				<div class="flex w-full flex-wrap items-center justify-between gap-2">
					<span>Could not load the saved identification job.</span>
					<button class="btn btn-sm" onclick={forgetJob}>Start a new check</button>
				</div>
			</div>
		{:else if !operation.data}
			<div class="mt-6 rounded-box border border-base-content/10 bg-base-200/40 p-4">
				<h3 class="font-semibold">Ready to check</h3>
				<p class="mt-1 text-sm text-base-content/60">
					The job continues on the server if you close this dialog.
				</p>
				<button
					class="btn btn-primary mt-4"
					disabled={start.isPending}
					onclick={() => void begin()}
				>
					{#if start.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
					Start identification
				</button>
				{#if start.isError}
					<p class="mt-3 text-sm text-error">Could not start identification. Try again.</p>
				{/if}
			</div>
		{:else}
			{@const job = operation.data}
			<div class="mt-5 rounded-box border border-primary/20 bg-base-200/35 p-4">
				<div class="flex flex-wrap items-center gap-2">
					<strong class="mr-auto capitalize">{job.state.replaceAll('_', ' ')}</strong>
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
				<progress
					class="progress progress-primary mt-3 w-full"
					value={job.completed_count}
					max={Math.max(1, job.expected_work_count)}
					aria-label="Identification progress"
				></progress>
				<p class="mt-2 text-xs text-base-content/60">
					{job.terminal_code
						? job.terminal_code.replaceAll('_', ' ').toLowerCase()
						: 'Checking local evidence'}
				</p>
			</div>

			{#if job.reidentification_candidates.length}
				<section class="mt-5" aria-labelledby="identification-candidates-title">
					<h3 id="identification-candidates-title" class="font-semibold">Candidates</h3>
					<div class="mt-3 grid gap-3 md:grid-cols-2">
						{#each job.reidentification_candidates as candidate (candidate.candidate_key)}
							<article class="rounded-box border border-base-content/10 bg-base-100 p-4">
								<div class="flex items-start gap-2">
									<div class="min-w-0 flex-1">
										<h4 class="font-semibold">{candidate.evidence.album_title}</h4>
										<p class="text-sm text-base-content/60">
											{candidate.evidence.album_artist_name} · score {candidate.evidence.score.toFixed(
												2
											)}
										</p>
									</div>
									{#if candidate.automatic_safe}<span class="badge badge-success badge-sm"
											>Strong evidence</span
										>{/if}
								</div>
								<dl class="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
									<dt class="text-base-content/55">Album title</dt>
									<dd>{evidenceLabel(candidate.evidence.album_title_classification)}</dd>
									<dt class="text-base-content/55">Album artist</dt>
									<dd>{evidenceLabel(candidate.evidence.album_artist_classification)}</dd>
									<dt class="text-base-content/55">Track evidence</dt>
									<dd>
										{countEvidence(candidate, 'supported')} supported,
										{countEvidence(candidate, 'contradictory')} conflicting,
										{countEvidence(candidate, 'unknown')} unknown
									</dd>
								</dl>
								<div class="mt-3 rounded-lg bg-base-200 p-2 text-xs">
									<p class="font-medium">External IDs to attach</p>
									<code class="mt-1 block break-all"
										>Album: {candidate.evidence.release_group_mbid}</code
									>
									{#if candidate.evidence.release_mbid}<code class="block break-all"
											>Release: {candidate.evidence.release_mbid}</code
										>{/if}
								</div>
								{#if countEvidence(candidate, 'contradictory')}
									<div class="mt-3 text-xs text-warning">
										<p class="font-medium">Conflicting tracks</p>
										<ul class="mt-1 list-disc pl-4">
											{#each candidate.evidence.track_evidence.filter((item) => item.classification === 'contradictory') as item (item.local_track_id)}
												<li>{item.candidate_track_title ?? item.local_track_id}</li>
											{/each}
										</ul>
									</div>
								{/if}
								{#if countEvidence(candidate, 'unknown') || candidate.evidence.unmatched_expected_tracks.length}
									<p class="mt-3 text-xs text-base-content/60">
										{countEvidence(candidate, 'unknown')} local tracks have unknown evidence;
										{candidate.evidence.unmatched_expected_tracks.length} expected release tracks are
										missing.
									</p>
								{/if}
								<button
									class="btn btn-primary btn-sm mt-3"
									disabled={job.state !== 'ready' || selectCandidate.isPending}
									onclick={(event) => chooseCandidate(candidate, event)}
									>{candidate.automatic_safe ? 'Use this identity' : 'Review and use...'}</button
								>
							</article>
						{/each}
					</div>
				</section>
			{/if}
			{#if selectCandidate.isError}
				<div class="alert alert-warning mt-4 text-sm">
					The candidate evidence changed. Review the current candidates before choosing again.
				</div>
			{/if}
			{#if ['succeeded', 'failed', 'cancelled', 'stopped'].includes(job.state)}
				<button class="btn btn-outline btn-sm mt-4" onclick={forgetJob}>Start another check</button>
			{/if}
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>

<dialog
	bind:this={confirmationDialog}
	class="modal"
	aria-labelledby="identification-confirm-title"
	onclose={() => confirmationOpener?.focus()}
>
	<div class="modal-box max-w-xl">
		<h2
			bind:this={confirmationHeading}
			id="identification-confirm-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Use this identity despite conflicting evidence?
		</h2>
		{#if confirmationCandidate}
			<p class="mt-3 text-sm text-base-content/70">
				This choice will replace the attached external album identity with
				<strong>{confirmationCandidate.evidence.album_title}</strong>. Your local files and tags
				will stay as they are.
			</p>
			<div class="mt-4 space-y-4 rounded-box border border-warning/30 bg-warning/10 p-3 text-sm">
				<section>
					<h3 class="font-semibold">Why this needs confirmation</h3>
					<p>{reasonLabel(confirmationCandidate.evidence.reason_code)}</p>
				</section>
				<section>
					<h3 class="font-semibold">Failed evidence gates</h3>
					<ul class="mt-1 list-disc pl-5 text-xs">
						{#if confirmationCandidate.evidence.album_title_classification !== 'supported'}
							<li>
								Album title: {evidenceLabel(
									confirmationCandidate.evidence.album_title_classification
								)}
							</li>
						{/if}
						{#if confirmationCandidate.evidence.album_artist_classification !== 'supported'}
							<li>
								Album artist: {evidenceLabel(
									confirmationCandidate.evidence.album_artist_classification
								)}
							</li>
						{/if}
						{#if confirmationCandidate.evidence.unmatched_expected_tracks.length}
							<li>
								{confirmationCandidate.evidence.unmatched_expected_tracks.length} expected release tracks
								are missing locally
							</li>
						{/if}
					</ul>
				</section>
				{#if countEvidence(confirmationCandidate, 'contradictory')}
					<section>
						<h3 class="font-semibold">Contradictory local tracks</h3>
						<ul class="mt-1 list-disc pl-5 text-xs">
							{#each confirmationCandidate.evidence.track_evidence.filter((item) => item.classification === 'contradictory') as item (item.local_track_id)}
								<li>
									<code>{item.local_track_id}</code>{#if item.candidate_track_title}
										- candidate track: {item.candidate_track_title}{/if}
								</li>
							{/each}
						</ul>
					</section>
				{/if}
				{#if countEvidence(confirmationCandidate, 'unknown')}
					<section>
						<h3 class="font-semibold">Unknown local tracks</h3>
						<ul class="mt-1 list-disc pl-5 text-xs">
							{#each confirmationCandidate.evidence.track_evidence.filter((item) => item.classification === 'unknown') as item (item.local_track_id)}
								<li><code>{item.local_track_id}</code></li>
							{/each}
						</ul>
					</section>
				{/if}
				<section>
					<h3 class="font-semibold">External identities that will attach</h3>
					<code class="mt-1 block break-all text-xs"
						>Release group: {confirmationCandidate.evidence.release_group_mbid}</code
					>
					{#if confirmationCandidate.evidence.release_mbid}<code class="block break-all text-xs"
							>Release: {confirmationCandidate.evidence.release_mbid}</code
						>{/if}
					<ul class="mt-2 space-y-1 text-xs">
						{#each confirmationCandidate.evidence.track_evidence.filter((item) => item.classification === 'supported' && item.recording_mbid) as item (item.local_track_id)}
							<li><code>{item.local_track_id}</code> → <code>{item.recording_mbid}</code></li>
						{/each}
					</ul>
				</section>
				{#if album.musicbrainz_release_group_id}
					<code class="block break-all text-xs"
						>Current album ID: {album.musicbrainz_release_group_id}</code
					>
				{/if}
				<p class="text-xs">
					This becomes a durable manual identity. Later scans will preserve it until an
					administrator resets it.
				</p>
			</div>
		{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => confirmationDialog.close()}>Cancel</button>
			<button
				class="btn btn-warning"
				disabled={selectCandidate.isPending}
				onclick={() => void confirmCandidate()}>Use conflicting identity</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close conflicting identity confirmation">close</button>
	</form>
</dialog>
