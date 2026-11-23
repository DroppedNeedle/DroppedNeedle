<!--
	Everything the person told Discover that is in effect, newest first: what
	it is about, the answer in words, when they said it, when it lapses, and
	Undo. How they tuned their picks ("Less from this") follows, each with
	its Undo.
-->
<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import {
		createUndoDiscoverFeedbackMutation,
		createUndoTuningMutation
	} from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import {
		getDiscoverFeedbackV3Query,
		getTuningsQuery
	} from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import { FEEDBACK_WORDS, dayAndMonth } from '$lib/features/discover/state/passLine';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	interface Props {
		open: boolean;
	}

	let { open = $bindable(false) }: Props = $props();

	const toldQuery = getDiscoverFeedbackV3Query(() => open);
	const tuningsQuery = getTuningsQuery(() => open);
	const undo = createUndoDiscoverFeedbackMutation();
	const undoTuning = createUndoTuningMutation();
	// Both reads answered, and neither holds anything.
	const nothing = $derived(
		toldQuery.data?.items.length === 0 && tuningsQuery.data?.items.length === 0
	);
</script>

<Modal bind:open title="What you told Discover" closeButton>
	{#if nothing}
		<p class="py-6 text-center text-sm text-muted">Nothing yet.</p>
	{/if}
	<QueryBoundary query={toldQuery} what="what you told Discover">
		{#snippet children(data)}
			<ul class="divide-y divide-line">
				{#each data.items as told (`${told.target}:${told.target_mbid}`)}
					{@const name = told.target === 'artist' ? told.artist_name : told.release_name}
					<li class="flex items-center gap-3 py-2">
						{#if told.target === 'artist'}
							<ArtistImage
								mbid={told.target_mbid}
								alt={told.artist_name}
								size="sm"
								rounded="full"
								className="shrink-0"
							/>
						{:else}
							<AlbumImage
								mbid={told.target_mbid}
								alt={told.release_name}
								size="sm"
								className="shrink-0"
							/>
						{/if}
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-bold">{name || told.target_mbid}</p>
							{#if told.target === 'album' && told.artist_name}
								<p class="truncate font-mono text-xs text-muted">{told.artist_name}</p>
							{/if}
							<p class="truncate text-xs text-muted">
								{FEEDBACK_WORDS[told.kind]} · {dayAndMonth(told.created_at)} ·
								{told.expires_at
									? `Back after ${dayAndMonth(told.expires_at)}`
									: 'Until you undo it'}
							</p>
						</div>
						<AsyncButton
							variant="outline"
							size="sm"
							aria-label="Undo for {name || told.target_mbid}"
							onclick={() =>
								undo
									.mutateAsync({ target: told.target, mbid: told.target_mbid })
									.catch(() => undefined)}
						>
							Undo
						</AsyncButton>
					</li>
				{/each}
			</ul>
		{/snippet}
	</QueryBoundary>
	<QueryBoundary query={tuningsQuery} what="how you tuned Discover">
		{#snippet children(data)}
			<ul class="divide-y divide-line">
				{#each data.items as tuning (`${tuning.kind}:${tuning.target}`)}
					{@const name = tuning.name || tuning.target}
					<li class="flex items-center gap-3 py-2">
						<ArtistImage
							mbid={tuning.target}
							alt={name}
							size="sm"
							rounded="full"
							className="shrink-0"
						/>
						<div class="min-w-0 flex-1">
							<p class="truncate text-sm font-bold">{name}</p>
							<p class="truncate text-xs text-muted">
								Less from this · {dayAndMonth(tuning.created_at)} · Until you undo it
							</p>
						</div>
						<AsyncButton
							variant="outline"
							size="sm"
							aria-label="Undo less from {name}"
							onclick={() =>
								undoTuning
									.mutateAsync({ kind: tuning.kind, target: tuning.target })
									.catch(() => undefined)}
						>
							Undo
						</AsyncButton>
					</li>
				{/each}
			</ul>
		{/snippet}
	</QueryBoundary>
</Modal>
