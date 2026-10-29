<!--
	What an opened review rests on: the album's cover and current edition,
	each release it could be with how every file fits (and the MusicBrainz
	recording it matched), and the album's history.
-->
<script lang="ts">
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import AlbumHistory from '$lib/components/library/AlbumHistory.svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getAlbumEditionStatusQuery,
		type EditionStatus
	} from '$lib/queries/albums/EditionQueries.svelte';
	import type { LibraryReview } from '$lib/queries/library/LibraryReviewQueries.svelte';

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
		unmatched: 'Keeps its own tags',
		unidentified: 'Not identified'
	};

	function percent(score: number): string {
		return `${Math.round(Math.max(0, Math.min(1, score)) * 100)}%`;
	}

	function short(mbid: string): string {
		return mbid.slice(0, 8);
	}

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
					{#if edition.release_mbid}
						<a
							class="link font-mono text-xs text-base-content/60"
							href={outsideLink(`https://musicbrainz.org/release/${edition.release_mbid}`)}
							target="_blank"
							rel="noopener noreferrer">Release {short(edition.release_mbid)}</a
						>
					{/if}
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
								{candidate.album_artist_name}
								{#if candidate.score > 0}· {percent(candidate.score)} fit{/if}
								{#if candidate.supported_tracks}· {candidate.supported_tracks} agree{/if}
								{#if candidate.contradictory_tracks}· {candidate.contradictory_tracks} disagree{/if}
							</p>
						</div>
						{#if authStore.isTrusted && review.state === 'pending'}
							<Button
								variant="primary"
								size="xs"
								disabled={busy}
								onclick={() => onchoose(candidate.candidate_key)}>Use this release</Button
							>
						{/if}
					</div>
					{#if candidate.track_evidence.length}
						<details class="mt-1 text-xs">
							<summary class="cursor-pointer text-base-content/60">How each file fits</summary>
							<ul class="mt-1 space-y-1">
								{#each candidate.track_evidence as evidence (evidence.local_track_id)}
									<li class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
										<span
											class="badge badge-xs {evidence.classification === 'supported'
												? 'badge-success'
												: evidence.classification === 'contradictory'
													? 'badge-error'
													: 'badge-ghost'}">{evidence.classification}</span
										>
										<span class="min-w-0 truncate">{evidence.title || evidence.local_track_id}</span
										>
										{#if evidence.evidence_kinds.length}<span class="text-base-content/50"
												>({evidence.evidence_kinds.join(', ')})</span
											>{/if}
										{#if evidence.recording_mbid}<a
												class="link font-mono text-base-content/60"
												title={evidence.recording_mbid}
												href={outsideLink(
													`https://musicbrainz.org/recording/${evidence.recording_mbid}`
												)}
												target="_blank"
												rel="noopener noreferrer">Recording {short(evidence.recording_mbid)}</a
											>{/if}
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
			No candidates were kept with this review. Retry identification, or pick an edition from the
			album page.
		</p>
	{/if}

	<AlbumHistory albumId={review.album_id} />
</div>
