<!--
	One identification review: the album, why it waits, and when opened the
	releases it could be and the actions the caller may take.
-->
<script lang="ts">
	import { ChevronDown, ChevronRight } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { LibraryReview } from '$lib/queries/library/LibraryReviewQueries.svelte';
	import type { ReviewAction } from '$lib/queries/library/LibraryReviewMutations.svelte';

	interface Props {
		review: LibraryReview;
		open: boolean;
		/** Shows the selection box. */
		selectable: boolean;
		selected: boolean;
		/** Every match is selected, so single boxes are locked on. */
		lockedSelected: boolean;
		busy: boolean;
		ontoggleopen: () => void;
		ontoggleselect: () => void;
		onaction: (action: ReviewAction) => void;
		onchoose: (candidateKey: string) => void;
		onretry: () => void;
	}

	let {
		review,
		open,
		selectable,
		selected,
		lockedSelected,
		busy,
		ontoggleopen,
		ontoggleselect,
		onaction,
		onchoose,
		onretry
	}: Props = $props();

	const STATE_LABEL: Record<LibraryReview['state'], string> = {
		pending: 'Needs a decision',
		approved: 'Resolved',
		dismissed: 'Closed',
		rejected: 'Kept as tagged',
		excluded: 'Excluded'
	};

	function percent(score: number): string {
		return `${Math.round(Math.max(0, Math.min(1, score)) * 100)}%`;
	}
</script>

<li class="rounded-box border border-base-content/10 bg-base-100">
	<div class="flex items-start gap-3 p-3">
		{#if selectable}
			<input
				type="checkbox"
				class="checkbox checkbox-sm mt-0.5"
				aria-label={`Select ${review.album_title}`}
				checked={lockedSelected || selected}
				disabled={lockedSelected}
				onchange={ontoggleselect}
			/>
		{/if}
		<button
			class="flex min-w-0 flex-1 items-start gap-2 text-left"
			aria-expanded={open}
			onclick={ontoggleopen}
		>
			{#if open}<ChevronDown class="mt-0.5 h-4 w-4 shrink-0" />{:else}<ChevronRight
					class="mt-0.5 h-4 w-4 shrink-0"
				/>{/if}
			<span class="min-w-0">
				<span class="block truncate font-medium">{review.album_title || 'Unknown album'}</span>
				<span class="block truncate text-xs text-base-content/55">{review.artist_name}</span>
				<span class="mt-1 block text-sm text-base-content/70">{review.reason.message}</span>
				<span class="block text-xs text-base-content/55">{review.reason.action}</span>
			</span>
		</button>
		<div class="flex shrink-0 flex-col items-end gap-2 sm:flex-row sm:items-center">
			{#if review.state !== 'pending'}
				<span class="badge badge-ghost badge-sm whitespace-nowrap">{STATE_LABEL[review.state]}</span
				>
			{/if}
			<Button
				variant="ghost"
				size="sm"
				href={withBasePath(`/album/${encodeURIComponent(review.album_id)}`)}>Open album</Button
			>
		</div>
	</div>

	{#if open}
		<div class="space-y-3 border-t border-base-content/10 p-3">
			{#if review.candidates.length}
				<ul class="space-y-2">
					{#each review.candidates as candidate (candidate.candidate_key)}
						<li
							class="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-base-200 px-3 py-2"
						>
							<div class="min-w-0">
								<p class="truncate text-sm font-medium">{candidate.album_title}</p>
								<p class="truncate text-xs text-base-content/60">
									{candidate.album_artist_name}
									{#if candidate.score > 0}· {percent(candidate.score)} fit{/if}
									{#if candidate.supported_tracks}· {candidate.supported_tracks} tracks agree{/if}
									{#if candidate.contradictory_tracks}· {candidate.contradictory_tracks} disagree{/if}
								</p>
								{#if candidate.track_evidence.length}
									<details class="mt-1 text-xs">
										<summary class="cursor-pointer text-base-content/60">How each file fits</summary
										>
										<ul class="mt-1 space-y-0.5">
											{#each candidate.track_evidence as evidence (evidence.local_track_id)}
												<li class="flex items-center gap-2">
													<span
														class="badge badge-xs {evidence.classification === 'supported'
															? 'badge-success'
															: evidence.classification === 'contradictory'
																? 'badge-error'
																: 'badge-ghost'}">{evidence.classification}</span
													>
													<span class="truncate">{evidence.title || evidence.local_track_id}</span>
													{#if evidence.evidence_kinds.length}<span class="text-base-content/50"
															>({evidence.evidence_kinds.join(', ')})</span
														>{/if}
												</li>
											{/each}
										</ul>
									</details>
								{/if}
							</div>
							{#if authStore.isTrusted && review.state === 'pending'}
								<Button
									variant="primary"
									size="xs"
									disabled={busy}
									onclick={() => onchoose(candidate.candidate_key)}>Use this release</Button
								>
							{/if}
						</li>
					{/each}
				</ul>
			{:else}
				<p class="text-sm text-base-content/60">
					No candidates were kept with this review. Retry identification, or pick an edition from
					the album page.
				</p>
			{/if}

			<div class="flex flex-wrap gap-2">
				{#if review.state === 'pending'}
					{#if authStore.isTrusted}
						<Button size="sm" disabled={busy} onclick={() => onaction('keep_tagged')}
							>Keep as tagged</Button
						>
						<Button
							variant="ghost"
							size="sm"
							disabled={busy}
							title="Take the MusicBrainz match off; the album keeps its own tags"
							onclick={() => onaction('detach')}>Remove match</Button
						>
						<Button variant="ghost" size="sm" disabled={busy} onclick={() => onaction('dismiss')}
							>Close without deciding</Button
						>
					{/if}
					{#if authStore.isAdmin}
						<!-- Retrying would only meet the edition a person chose. -->
						{#if !review.manual_identity}
							<Button variant="ghost" size="sm" disabled={busy} onclick={onretry}
								>Retry identification</Button
							>
						{/if}
						<Button
							variant="danger-ghost"
							size="sm"
							disabled={busy}
							onclick={() => onaction('exclude')}>Take out of library</Button
						>
					{/if}
				{:else if review.state === 'excluded' && authStore.isAdmin}
					<Button size="sm" disabled={busy} onclick={() => onaction('restore')}
						>Put back in library</Button
					>
				{/if}
			</div>
		</div>
	{/if}
</li>
