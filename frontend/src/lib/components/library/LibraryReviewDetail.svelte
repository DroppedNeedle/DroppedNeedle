<script lang="ts">
	import { AlertTriangle, Check, ExternalLink, History, Music2, XCircle } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getLibraryReviewQuery } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import {
		acceptLibraryReviewCandidate,
		actOnLibraryReview,
		retryLibraryReview
	} from '$lib/queries/library/LibraryReviewMutations.svelte';
	import type {
		ReviewActionRequest,
		ReviewDetailResponse
	} from '$lib/queries/library/LibraryOperationsTypes';
	import { createUuid } from '$lib/utils/uuid';

	type ReviewCandidate = ReviewDetailResponse['candidates'][number];

	interface Props {
		reviewId: string | null;
		onclose: () => void;
	}
	let { reviewId, onclose }: Props = $props();
	const query = getLibraryReviewQuery(() => reviewId);
	const keep = actOnLibraryReview('keep_tagged');
	const detach = actOnLibraryReview('detach_keep_tagged');
	const exclude = actOnLibraryReview('exclude');
	const restore = actOnLibraryReview('restore');
	const accept = acceptLibraryReviewCandidate();
	const retry = retryLibraryReview();
	let dialog: HTMLDialogElement;
	let confirmDialog: HTMLDialogElement;
	let overrideDialog: HTMLDialogElement;
	let reviewHeading = $state<HTMLHeadingElement>();
	let confirmHeading: HTMLHeadingElement;
	let overrideHeading: HTMLHeadingElement;
	let confirmOpener: HTMLButtonElement | null = null;
	let overrideOpener: HTMLButtonElement | null = null;
	let focusedReviewId = $state<string | null>(null);
	let confirmation = $state<'detach' | 'exclude' | 'retry' | null>(null);
	let overrideCandidate = $state<ReviewCandidate | null>(null);

	$effect(() => {
		if (!dialog) return;
		if (reviewId && !dialog.open) dialog.showModal();
		if (reviewId && query.data && reviewHeading && focusedReviewId !== reviewId) {
			reviewHeading.focus();
			focusedReviewId = reviewId;
		}
		if (!reviewId && dialog.open) {
			dialog.close();
			focusedReviewId = null;
		}
	});

	function body(confirm = false): ReviewActionRequest | null {
		const detail = query.data;
		if (!detail) return null;
		return {
			expected_review_revision: detail.review.row_revision,
			expected_catalog_revision: detail.catalog_revision,
			expected_identity_revision: detail.identity_revision,
			expected_evidence_revision: detail.evidence_revision || null,
			idempotency_key: createUuid(),
			confirmation: confirm
		};
	}

	async function acceptCandidate(
		candidate: ReviewCandidate,
		manualOverride: boolean
	): Promise<void> {
		const detail = query.data;
		if (!detail || !reviewId) return;
		await accept.mutateAsync({
			reviewId,
			body: {
				expected_review_revision: detail.review.row_revision,
				expected_catalog_revision: detail.catalog_revision,
				expected_identity_revision: detail.identity_revision,
				expected_evidence_revision: candidate.evidence_revision,
				idempotency_key: createUuid(),
				confirmation: true,
				candidate_key: candidate.candidate_key,
				manual_override: manualOverride
			}
		});
	}

	function openOverrideConfirmation(
		candidate: ReviewCandidate,
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): void {
		overrideOpener = event.currentTarget;
		overrideCandidate = candidate;
		overrideDialog.showModal();
		overrideHeading.focus();
	}

	async function confirmOverride(): Promise<void> {
		if (!overrideCandidate) return;
		try {
			await acceptCandidate(overrideCandidate, true);
		} catch {
			return;
		}
		overrideDialog.close();
		overrideCandidate = null;
	}

	function openConfirmation(
		kind: 'detach' | 'exclude' | 'retry',
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): void {
		confirmOpener = event.currentTarget;
		confirmation = kind;
		confirmDialog.showModal();
		confirmHeading.focus();
	}

	async function confirmAction(): Promise<void> {
		const request = body(true);
		if (!request || !reviewId || !confirmation) return;
		try {
			if (confirmation === 'detach') await detach.mutateAsync({ reviewId, body: request });
			if (confirmation === 'exclude') await exclude.mutateAsync({ reviewId, body: request });
			if (confirmation === 'retry') await retry.mutateAsync({ reviewId, body: request });
		} catch {
			return;
		}
		confirmDialog.close();
	}

	function evidenceLabel(classification: string): string {
		if (classification === 'supported') return 'Supports this release';
		if (classification === 'contradictory') return 'Conflicts with this release';
		return 'Not enough information';
	}

	function countCandidateEvidence(candidate: ReviewCandidate, classification: string): number {
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

	function localTrackLabel(localTrackId: string): string {
		const track = query.data?.tracks.find((item) => item.id === localTrackId);
		return track ? `${track.title} (${localTrackId})` : localTrackId;
	}
</script>

<dialog bind:this={dialog} class="modal" {onclose} aria-labelledby="review-detail-title">
	<div class="modal-box h-[min(94dvh,58rem)] w-[min(96vw,76rem)] max-w-none overflow-y-auto p-0">
		{#if query.isLoading}
			<div class="space-y-4 p-6">
				<div class="skeleton h-24"></div>
				<div class="skeleton h-56"></div>
			</div>
		{:else if query.isError || !query.data}
			<div class="p-6"><div class="alert alert-error">Could not load this review item.</div></div>
		{:else}
			{@const detail = query.data}
			<header
				class="sticky top-0 z-10 flex items-start gap-4 border-b border-base-content/10 bg-base-100/95 p-5 backdrop-blur"
			>
				<AlbumImage
					mbid={detail.review.local_album_id ??
						detail.review.release_group_mbid ??
						detail.review.id}
					alt={`Cover for ${detail.review.album_title}`}
					size="sm"
					className="h-20 w-20 shrink-0"
				/>
				<div class="min-w-0 flex-1">
					<p class="font-mono text-xs uppercase tracking-wider text-primary/70">
						Identification review
					</p>
					<h2
						bind:this={reviewHeading}
						id="review-detail-title"
						tabindex="-1"
						class="truncate font-display text-2xl font-bold"
					>
						{detail.review.album_title || 'Untitled local album'}
					</h2>
					<p class="truncate text-base-content/60">
						{detail.review.album_artist_name || 'Unknown album artist'} · {detail.review
							.track_count} tracks
					</p>
					<a
						href={`/album/${detail.review.local_album_id}`}
						class="link link-hover mt-1 inline-flex items-center gap-1 text-xs"
						>Open local album <ExternalLink class="h-3 w-3" /></a
					>
				</div>
				<button
					class="btn btn-ghost btn-sm btn-square"
					onclick={() => dialog.close()}
					aria-label="Close review"><XCircle class="h-5 w-5" /></button
				>
			</header>

			<div class="space-y-6 p-5">
				<div class="alert alert-warning text-sm">
					<AlertTriangle class="h-4 w-4" /><span
						>{detail.review.reason_code === 'NO_CANDIDATE'
							? 'No external result'
							: detail.review.reason_code === 'AMBIGUOUS'
								? 'Several equally likely releases'
								: detail.review.reason_code === 'CONTRADICTORY'
									? 'Conflicting track evidence'
									: detail.review.reason_code.replaceAll('_', ' ').toLowerCase()}</span
					>
				</div>

				<section>
					<h3 class="mb-2 font-semibold">Current identity</h3>
					{#if detail.review.release_group_mbid}<div class="rounded-box bg-base-200 p-3 text-sm">
							<strong>MusicBrainz identity attached</strong><code
								class="mt-1 block select-all text-xs text-base-content/55"
								>{detail.review.release_group_mbid}</code
							>
						</div>{:else}<p class="text-sm text-base-content/55">
							Local metadata only. This album remains playable.
						</p>{/if}
				</section>

				<section>
					<h3 class="mb-3 font-semibold">Release candidates</h3>
					{#if detail.candidates.length === 0}<p
							class="rounded-box border border-dashed border-base-content/20 p-4 text-sm text-base-content/55"
						>
							No external result
						</p>{:else}<div class="grid gap-3 lg:grid-cols-2">
							{#each detail.candidates as candidate (candidate.candidate_key)}<article
									class="rounded-box border border-base-content/10 bg-base-100 p-4"
								>
									<div class="flex items-start justify-between gap-3">
										<div>
											<h4 class="font-semibold">{candidate.evidence.album_title}</h4>
											<p class="text-sm text-base-content/60">
												{candidate.evidence.album_artist_name}
											</p>
										</div>
										<span
											class="badge {candidate.automatic_safe
												? 'badge-success'
												: 'badge-warning'} badge-sm"
											>{candidate.automatic_safe ? 'Supported' : 'Review evidence'}</span
										>
									</div>
									<p class="mt-2 text-xs text-base-content/50">
										Score {candidate.evidence.score.toFixed(2)} · margin {candidate.evidence.margin.toFixed(
											2
										)}
									</p>
									<code class="mt-2 block select-all text-[0.7rem] text-base-content/45"
										>{candidate.evidence.release_group_mbid}</code
									><button
										class="btn btn-primary btn-sm mt-3"
										disabled={accept.isPending}
										onclick={(event) =>
											candidate.automatic_safe
												? void acceptCandidate(candidate, false).catch(() => undefined)
												: openOverrideConfirmation(candidate, event)}
										>{candidate.automatic_safe ? 'Use this release' : 'Use anyway...'}</button
									>
								</article>{/each}
						</div>{/if}
				</section>

				<section>
					<h3 class="mb-2 font-semibold">Track evidence</h3>
					<div class="overflow-x-auto rounded-box border border-base-content/10">
						<table class="table table-sm">
							<thead
								><tr><th>Track</th><th>Artist</th><th>Evidence</th><th>Recording ID</th></tr></thead
							><tbody
								>{#each detail.tracks as track (track.id)}{@const evidence = [
										...detail.supported,
										...detail.unknown,
										...detail.contradictory
									].find((item) => item.local_track_id === track.id)}<tr
										><td class="sticky left-0 bg-base-100"
											><Music2 class="mr-1 inline h-3.5 w-3.5" />
											{track.disc_number}.{track.track_number}
											{track.title}</td
										><td
											>{#if track.local_artist_id}<a
													class="link link-hover"
													href={`/artist/${track.local_artist_id}`}>{track.artist_name}</a
												>{:else}{track.artist_name || 'Unknown artist'}{/if}</td
										><td
											>{evidence
												? evidenceLabel(evidence.classification)
												: 'Not enough information'}</td
										><td><code class="text-[0.7rem]">{track.recording_mbid ?? '-'}</code></td></tr
									>{/each}</tbody
							>
						</table>
					</div>
				</section>

				<section>
					<h3 class="mb-2 flex items-center gap-2 font-semibold">
						<History class="h-4 w-4" /> Decision history
					</h3>
					{#if detail.history.length === 0}<p class="text-sm text-base-content/50">
							No earlier decisions.
						</p>{:else}<ol class="space-y-2">
							{#each detail.history as entry (entry.id)}<li
									class="flex items-center justify-between gap-3 rounded-lg bg-base-200 px-3 py-2 text-sm"
								>
									<span>{entry.state.replaceAll('_', ' ')}</span><time
										class="text-xs text-base-content/50"
										>{new Date(entry.created_at * 1000).toLocaleString()}</time
									>
								</li>{/each}
						</ol>{/if}
				</section>

				<div class="flex flex-wrap gap-2 border-t border-base-content/10 pt-4">
					{#if detail.available_actions.includes('keep_tagged')}<button
							class="btn btn-primary"
							disabled={keep.isPending}
							onclick={() => {
								const request = body();
								if (request)
									void keep
										.mutateAsync({ reviewId: detail.review.id, body: request })
										.catch(() => undefined);
							}}><Check class="h-4 w-4" /> Keep as tagged</button
						>{/if}
					{#if detail.available_actions.includes('detach_keep_tagged')}<button
							class="btn btn-outline"
							onclick={(event) => openConfirmation('detach', event)}
							>Detach and keep as tagged...</button
						>{/if}
					{#if detail.available_actions.includes('retry')}<button
							class="btn btn-outline"
							onclick={(event) => openConfirmation('retry', event)}>Retry identification</button
						>{/if}
					{#if detail.available_actions.includes('exclude')}<button
							class="btn btn-outline btn-error"
							onclick={(event) => openConfirmation('exclude', event)}>Exclude...</button
						>{/if}
					{#if detail.available_actions.includes('restore')}<button
							class="btn btn-outline"
							onclick={() => {
								const request = body();
								if (request)
									void restore
										.mutateAsync({ reviewId: detail.review.id, body: request })
										.catch(() => undefined);
							}}>Restore availability</button
						>{/if}
					<button class="btn btn-ghost" onclick={() => dialog.close()}>Leave for later</button>
				</div>
			</div>
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close review detail">close</button>
	</form>
</dialog>

<dialog
	bind:this={overrideDialog}
	class="modal"
	aria-labelledby="override-confirm-title"
	onclose={() => overrideOpener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2
			bind:this={overrideHeading}
			id="override-confirm-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Use this release despite conflicts?
		</h2>
		{#if overrideCandidate}
			<p class="mt-3 text-sm text-base-content/70">
				Choosing <strong>{overrideCandidate.evidence.album_title}</strong> will attach its external identity
				as a manual override. Local files and tags will not change.
			</p>
			<div class="mt-3 space-y-4 rounded-box border border-warning/30 bg-warning/10 p-3 text-sm">
				<section>
					<h3 class="font-semibold">Why this needs confirmation</h3>
					<p>{reasonLabel(overrideCandidate.evidence.reason_code)}</p>
				</section>
				<section>
					<h3 class="font-semibold">Failed evidence gates</h3>
					<ul class="mt-1 list-disc pl-5 text-xs">
						{#if overrideCandidate.evidence.album_title_classification !== 'supported'}
							<li>
								Album title: {evidenceLabel(overrideCandidate.evidence.album_title_classification)}
							</li>
						{/if}
						{#if overrideCandidate.evidence.album_artist_classification !== 'supported'}
							<li>
								Album artist: {evidenceLabel(
									overrideCandidate.evidence.album_artist_classification
								)}
							</li>
						{/if}
						{#if overrideCandidate.evidence.unmatched_expected_tracks.length}
							<li>
								{overrideCandidate.evidence.unmatched_expected_tracks.length} expected release tracks
								are missing locally
							</li>
						{/if}
					</ul>
				</section>
				{#if countCandidateEvidence(overrideCandidate, 'contradictory')}
					<section>
						<h3 class="font-semibold text-warning">Contradictory local tracks</h3>
						<ul class="mt-1 list-disc pl-5 text-xs">
							{#each overrideCandidate.evidence.track_evidence.filter((item) => item.classification === 'contradictory') as item (item.local_track_id)}
								<li>{localTrackLabel(item.local_track_id)}</li>
							{/each}
						</ul>
					</section>
				{/if}
				{#if countCandidateEvidence(overrideCandidate, 'unknown')}
					<section>
						<h3 class="font-semibold">Unknown local tracks</h3>
						<ul class="mt-1 list-disc pl-5 text-xs">
							{#each overrideCandidate.evidence.track_evidence.filter((item) => item.classification === 'unknown') as item (item.local_track_id)}
								<li>{localTrackLabel(item.local_track_id)}</li>
							{/each}
						</ul>
					</section>
				{/if}
				<section class="border-t border-warning/20 pt-3">
					<h3 class="font-semibold">External identities that will attach</h3>
					<code class="mt-1 block break-all text-xs"
						>Release group: {overrideCandidate.evidence.release_group_mbid}</code
					>
					{#if overrideCandidate.evidence.release_mbid}<code class="block break-all text-xs"
							>Release: {overrideCandidate.evidence.release_mbid}</code
						>{/if}
					<ul class="mt-2 space-y-1 text-xs">
						{#each overrideCandidate.evidence.track_evidence.filter((item) => item.classification === 'supported' && item.recording_mbid) as item (item.local_track_id)}
							<li>{localTrackLabel(item.local_track_id)} → <code>{item.recording_mbid}</code></li>
						{/each}
					</ul>
					{#if query.data?.review.release_group_mbid}
						<code class="block break-all text-xs"
							>Current album ID: {query.data.review.release_group_mbid}</code
						>
					{:else}
						<p class="text-xs text-base-content/60">No external album ID is currently attached.</p>
					{/if}
					<p class="mt-2 text-xs text-base-content/60">
						This becomes a durable manual identity. Later scans will preserve it until an
						administrator resets it. Contradictory and unknown tracks remain local-only.
					</p>
				</section>
			</div>
		{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => overrideDialog.close()}>Cancel</button>
			<button
				class="btn btn-warning"
				disabled={accept.isPending}
				onclick={() => void confirmOverride()}>Use conflicting release</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close manual override confirmation">close</button>
	</form>
</dialog>

<dialog
	bind:this={confirmDialog}
	class="modal"
	aria-labelledby="review-confirm-title"
	onclose={() => confirmOpener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2
			bind:this={confirmHeading}
			id="review-confirm-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			{confirmation === 'detach'
				? 'Detach identity and keep local metadata?'
				: confirmation === 'exclude'
					? 'Exclude this album?'
					: 'Retry identification?'}
		</h2>
		{#if confirmation === 'detach' && query.data}
			<div class="mt-3 space-y-3 text-sm text-base-content/70">
				<p>
					Detach <strong>{query.data.review.album_title}</strong> (<code
						>{query.data.review.local_album_id}</code
					>) from release group <code>{query.data.review.release_group_mbid}</code>.
				</p>
				{#if query.data.tracks.some((track) => track.recording_mbid)}
					<section>
						<h3 class="font-semibold text-base-content">Track identities that will be removed</h3>
						<ul class="mt-1 list-disc space-y-1 pl-5 text-xs">
							{#each query.data.tracks.filter((track) => track.recording_mbid) as track (track.id)}
								<li>
									{track.title} (<code>{track.id}</code>) → <code>{track.recording_mbid}</code>
								</li>
							{/each}
						</ul>
					</section>
				{/if}
				<p>
					The album and track local IDs, playback, playlists, history, favorites, and artwork will
					remain.
				</p>
			</div>
		{:else if confirmation === 'exclude'}<p class="mt-3 text-sm text-base-content/70">
				This album will be hidden from DroppedNeedle and connected music clients. Files remain on
				disk and the decision can be reversed.
			</p>{:else}<p class="mt-3 text-sm text-base-content/70">
				The current files will be checked again in a tracked background job. Existing playback stays
				available.
			</p>{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => confirmDialog.close()}>Cancel</button><button
				class="btn {confirmation === 'exclude' ? 'btn-error' : 'btn-primary'}"
				onclick={() => void confirmAction()}
				>{confirmation === 'detach'
					? 'Detach identity and keep local metadata'
					: confirmation === 'exclude'
						? 'Exclude album'
						: 'Retry identification'}</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close confirmation">close</button>
	</form>
</dialog>
