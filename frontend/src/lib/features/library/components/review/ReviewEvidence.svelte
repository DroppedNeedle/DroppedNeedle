<!--
	What an opened review rests on: the album's cover and current edition,
	each edition it could be with how every file fits, in words, and the
	album's history.
-->
<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AlbumHistory from '$lib/features/library/components/AlbumHistory.svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getAlbumEditionStatusQuery,
		type EditionStatus
	} from '$lib/features/library/queries/EditionQueries.svelte';
	import type { LibraryReview } from '$lib/features/library/queries/LibraryReviewQueries.svelte';
	import { fitLine } from '../edition/editionCopy';

	interface Props {
		review: LibraryReview;
		busy: boolean;
		onchoose: (candidateKey: string) => void;
	}

	let { review, busy, onchoose }: Props = $props();

	const status = getAlbumEditionStatusQuery(
		() => authStore.user?.id,
		() => review.album_id
	);

	const IDENTITY: Record<EditionStatus['state'], string> = {
		chosen: 'Chosen by a person',
		confirmed: 'Picked by DroppedNeedle',
		unconfirmed: 'Best guess',
		unmatched: 'Not found',
		unidentified: 'Not identified'
	};

	const FIT: Record<string, { label: string; tone: string }> = {
		supported: { label: 'Fits', tone: 'badge-success' },
		contradictory: { label: "Doesn't fit", tone: 'badge-error' }
	};
	const UNSURE = { label: 'Unsure', tone: 'badge-ghost' };

	const current = $derived(status.data?.release_mbid ?? null);
</script>

<div class="space-y-3">
	<div class="flex items-start gap-3">
		<AlbumImage
			albumId={review.album_id}
			mbid={status.data?.release_group_mbid ?? ''}
			alt=""
			size="sm"
			rounded="md"
			className="h-16 w-16 shrink-0 border border-base-content/10"
		/>
		<div class="min-w-0 text-sm">
			<QueryBoundary query={status} what="the album's edition">
				{#snippet loading()}
					<div class="skeleton h-4 w-40"></div>
					<div class="mt-2 skeleton h-3 w-56 max-w-full"></div>
				{/snippet}
				{#snippet children(edition)}
					<p class="font-medium">{IDENTITY[edition.state]}</p>
					<p class="text-xs text-base-content/60">{edition.reason.message}</p>
				{/snippet}
			</QueryBoundary>
		</div>
	</div>

	{#if review.candidates.length}
		<ul class="space-y-2">
			{#each review.candidates as candidate (candidate.candidate_key)}
				<li class="rounded-lg bg-base-200 px-3 py-2">
					<div class="flex flex-wrap items-center justify-between gap-2">
						<div class="min-w-0">
							<p class="truncate text-sm font-medium">
								{candidate.album_title}
								{#if current && candidate.release_mbid === current}<span
										class="badge badge-sm badge-ghost ml-1">Current</span
									>{/if}
							</p>
							<p class="truncate text-xs text-base-content/60">
								{[
									candidate.album_artist_name,
									fitLine(candidate.supported_tracks, candidate.track_evidence.length),
									candidate.contradictory_tracks
										? `${candidate.contradictory_tracks} don't fit`
										: null
								]
									.filter(Boolean)
									.join(' · ')}
							</p>
						</div>
						{#if authStore.isTrusted && review.state === 'pending'}
							<Button
								variant="primary"
								size="xs"
								disabled={busy}
								onclick={() => onchoose(candidate.candidate_key)}>Use this edition</Button
							>
						{/if}
					</div>
					{#if candidate.track_evidence.length}
						<details class="mt-1 text-xs">
							<summary class="cursor-pointer text-base-content/60">How each file fits</summary>
							<ul class="mt-1 space-y-1">
								{#each candidate.track_evidence as evidence (evidence.local_track_id)}
									{@const fit = FIT[evidence.classification] ?? UNSURE}
									<li class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
										<span class="badge badge-xs {fit.tone}">{fit.label}</span>
										<span class="min-w-0 truncate">{evidence.title || 'Untitled file'}</span>
									</li>
								{/each}
							</ul>
						</details>
					{/if}
				</li>
			{/each}
		</ul>
	{:else}
		<p class="text-sm text-base-content/60">
			No candidates were kept with this review. Look again, or pick an edition from the album page.
		</p>
	{/if}

	<AlbumHistory albumId={review.album_id} />
</div>
