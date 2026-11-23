<!--
	One plain user's playlist press, waiting for approval as one card. Closed
	it is a row with the first covers, the playlist's name and who asked,
	and Approve all / Reject all. Opened it lists every waiting album with a
	tick; the approve button counts the ticked ones, and the unticked are
	rejected in the same decision. On a phone the buttons go full width
	under the list.
-->
<script lang="ts">
	import { SvelteSet } from 'svelte/reactivity';
	import { Check, ChevronDown, ChevronUp, X } from 'lucide-svelte';
	import type { components } from '$lib/api/v3/openapi';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { createDecideApprovalBatchMutation } from '$lib/features/requests/queries/RequestMutations.svelte';
	import './requests.css';

	type ApprovalCard = components['schemas']['ApprovalCard'];

	interface Props {
		card: ApprovalCard;
	}

	let { card }: Props = $props();

	const decideMutation = createDecideApprovalBatchMutation();

	let open = $state(false);
	// The albums an admin unticked; every other waiting album is approved.
	const unticked = new SvelteSet<string>();

	const ticked = $derived(card.items.filter((item) => !unticked.has(item.musicbrainz_id)));
	const left = $derived(card.items.length - ticked.length);
	const date = $derived(new Date(card.created_at).toLocaleDateString());

	function toggle(id: string) {
		if (unticked.has(id)) unticked.delete(id);
		else unticked.add(id);
	}

	function toggleAll() {
		if (left === 0) card.items.forEach((item) => unticked.add(item.musicbrainz_id));
		else unticked.clear();
	}

	function decide(approve: string[]) {
		decideMutation.mutate({
			batchId: card.id,
			approve,
			all: card.items.map((item) => item.musicbrainz_id)
		});
	}
</script>

<Card variant="bordered" padding="none" class="rq-row" aria-label="Requests from {card.name}">
	<div class="ab-card">
		<div class="ab-mosaic" aria-hidden="true">
			{#each card.items.slice(0, 4) as item (item.musicbrainz_id)}
				<AlbumImage
					mbid={item.musicbrainz_id}
					customUrl={item.cover_url}
					alt=""
					size="full"
					rounded="none"
					className="h-full w-full"
				/>
			{/each}
		</div>

		<div class="ab-body">
			<div class="flex min-w-0 items-center gap-2.5">
				<span class="rq-pill rq-pill--neutral rq-pill--outline">Playlist</span>
				<p class="rq-title">{card.name}</p>
			</div>
			<p class="rq-meta mt-2">
				<span>{card.waiting} {card.waiting === 1 ? 'album' : 'albums'}</span>
				<span>Requested by {card.requester_name}</span>
				<span>{date}</span>
			</p>
			<Button
				variant="ghost"
				size="sm"
				class="mt-2 -ml-2"
				icon={open ? ChevronUp : ChevronDown}
				aria-expanded={open}
				onclick={() => (open = !open)}
			>
				{open ? 'Hide the albums' : `Show the ${card.waiting} albums`}
			</Button>
		</div>

		<div class="ab-actions">
			<div class="ab-buttons">
				<Button
					variant="primary"
					size="display-sm"
					icon={Check}
					disabled={ticked.length === 0}
					loading={decideMutation.isPending}
					onclick={() => decide(ticked.map((item) => item.musicbrainz_id))}
				>
					{left === 0 ? `Approve all ${card.items.length}` : `Approve ${ticked.length}`}
				</Button>
				<Button
					variant="danger-ghost"
					size="display-sm"
					icon={X}
					disabled={decideMutation.isPending}
					onclick={() => decide([])}
				>
					Reject all
				</Button>
			</div>
			{#if left > 0 && ticked.length > 0}
				<p class="text-sm text-muted">The {left} unticked {left === 1 ? 'is' : 'are'} rejected.</p>
			{/if}
		</div>

		{#if open}
			<div class="ab-list">
				<label class="flex items-center gap-3 text-sm font-medium">
					<input
						type="checkbox"
						class="checkbox checkbox-sm"
						checked={left === 0}
						indeterminate={left > 0 && ticked.length > 0}
						onchange={toggleAll}
					/>
					{ticked.length} of {card.items.length} ticked
				</label>
				<ul class="mt-3 flex flex-col divide-y divide-line">
					{#each card.items as item (item.musicbrainz_id)}
						<li class="flex items-center gap-3 py-2">
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={!unticked.has(item.musicbrainz_id)}
								aria-label="Approve {item.title}"
								onchange={() => toggle(item.musicbrainz_id)}
							/>
							<AlbumImage
								mbid={item.musicbrainz_id}
								customUrl={item.cover_url}
								alt=""
								size="xs"
								rounded="sm"
								className="size-10 shrink-0"
							/>
							<span class="flex min-w-0 flex-1 flex-col">
								<span class="truncate text-sm font-semibold">{item.title}</span>
								<span class="truncate text-sm text-muted">{item.artist_name}</span>
							</span>
							{#if item.song_count}
								<span class="shrink-0 text-sm text-muted">
									{item.song_count}
									{item.song_count === 1 ? 'song' : 'songs'}
								</span>
							{/if}
						</li>
					{/each}
				</ul>
			</div>
		{/if}
	</div>
</Card>

<style>
	.ab-card {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		grid-template-areas:
			'mosaic body'
			'list list'
			'actions actions';
		gap: 1rem;
		padding: 1rem;
	}
	.ab-mosaic {
		grid-area: mosaic;
		display: grid;
		grid-template-columns: repeat(2, 1fr);
		width: 5.5rem;
		height: 5.5rem;
		overflow: hidden;
		border-radius: var(--radius-box);
	}
	.ab-body {
		grid-area: body;
		min-width: 0;
	}
	.ab-actions {
		grid-area: actions;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.ab-buttons {
		display: flex;
		flex-direction: column;
		gap: 0.625rem;
	}
	.ab-buttons :global(.btn) {
		width: 100%;
	}
	.ab-list {
		grid-area: list;
	}
	@media (min-width: 640px) {
		.ab-card {
			grid-template-columns: auto minmax(0, 1fr) auto;
			grid-template-areas:
				'mosaic body actions'
				'list list list';
		}
		.ab-actions {
			align-items: flex-end;
		}
		.ab-buttons {
			flex-direction: row;
			flex-wrap: wrap;
			justify-content: flex-end;
		}
		.ab-buttons :global(.btn) {
			width: auto;
		}
	}
</style>
