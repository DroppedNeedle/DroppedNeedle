<!--
	Name artists you love: a search field with no button to press first.
	Typing lists artists from search as rows (round picture, name,
	MusicBrainz's disambiguation or "In your library", and Add). Add names
	the artist at once and puts its name tag under the field; a tag's remove
	button takes it back. `first` shows the three squares a first set needs,
	for the empty Discover page. At the limit the field says so.
-->
<script lang="ts">
	import { Check, Plus, Search } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import NameTag from '$lib/ui/NameTag.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { getSearchSuggestionsV3Query } from '$lib/features/search/queries/SearchV3Queries.svelte';
	import {
		createNameStatedArtistMutation,
		createRemoveStatedArtistMutation,
		getStatedArtistsQuery,
		type StatedArtist
	} from '$lib/features/discover/queries/statedArtists.svelte';

	interface Props {
		/** Show the squares toward a first set. */
		first?: boolean;
	}

	let { first = false }: Props = $props();

	/** Named artists a first set needs. */
	const FIRST_SET = 3;
	/** The search bar's own fetch, so both share one answer. */
	const SUGGEST_LIMIT = 10;
	const DEBOUNCE_MS = 300;

	const statedQuery = getStatedArtistsQuery();
	const nameArtist = createNameStatedArtistMutation();
	const removeArtist = createRemoveStatedArtistMutation();

	let query = $state('');
	let searched = $state('');
	let timer: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const typed = query.trim();
		clearTimeout(timer);
		timer = setTimeout(() => (searched = typed.length >= 2 ? typed : ''), DEBOUNCE_MS);
		return () => clearTimeout(timer);
	});

	const suggestQuery = getSearchSuggestionsV3Query(
		() => searched,
		() => searched.length >= 2,
		SUGGEST_LIMIT
	);

	const named = $derived(
		new Set((statedQuery.data?.items ?? []).map((artist) => artist.artist_mbid.toLowerCase()))
	);
	let adding = $state<string | null>(null);
	let removing = $state<string | null>(null);

	async function add(mbid: string, name: string) {
		adding = mbid;
		try {
			await nameArtist.mutateAsync({ mbid, name });
		} catch {
			// The mutation says so itself.
		} finally {
			adding = null;
		}
	}

	async function remove(mbid: string) {
		removing = mbid;
		try {
			await removeArtist.mutateAsync(mbid);
		} catch {
			// The mutation says so itself.
		} finally {
			removing = null;
		}
	}
</script>

<div class="dn-stated">
	<QueryBoundary query={statedQuery} what="the artists you named">
		{#snippet children(data)}
			{@render picker(data.items, data.limit)}
		{/snippet}
	</QueryBoundary>
</div>

{#snippet picker(stated: StatedArtist[], limit: number)}
	{@const full = stated.length >= limit}
	<Input
		bind:value={query}
		type="search"
		icon={Search}
		paper
		label="Name an artist you love"
		placeholder={full ? `${limit} named, the most there is` : 'Name an artist you love'}
		disabled={full}
		autocomplete="off"
		class="w-full"
	/>
	{#if full}
		<p class="dn-stated__note">That is {limit}, the most. Remove one to name another.</p>
	{:else if searched}
		<QueryBoundary query={suggestQuery} what="artists">
			{#snippet children(data)}
				{@const artists = data.results.filter(
					(result) => result.kind === 'artist' && result.musicbrainz_id
				)}
				{#if artists.length === 0}
					<p class="dn-stated__note">No artist found by that name.</p>
				{:else}
					<ul class="dn-stated__rows" aria-label="Artists found">
						{#each artists as artist (artist.musicbrainz_id)}
							{@const mbid = artist.musicbrainz_id ?? ''}
							{@const done = named.has(mbid.toLowerCase())}
							<li class="dn-stated__row">
								<ArtistImage {mbid} alt="" size="xs" rounded="full" className="shrink-0" />
								<div class="dn-stated__what">
									<p class="dn-stated__name">{artist.title}</p>
									{#if artist.disambiguation}
										<p class="dn-stated__sub">{artist.disambiguation}</p>
									{:else if artist.in_library}
										<p class="dn-stated__sub">In your library</p>
									{/if}
								</div>
								<Button
									variant="paper"
									size="sm"
									icon={done ? Check : Plus}
									disabled={done || adding === mbid}
									aria-label={done ? `${artist.title} added` : `Add ${artist.title}`}
									onclick={() => void add(mbid, artist.title)}
								>
									{done ? 'Added' : 'Add'}
								</Button>
							</li>
						{/each}
					</ul>
				{/if}
			{/snippet}
		</QueryBoundary>
	{/if}

	{#if stated.length}
		<ul class="dn-stated__tags" aria-label="Artists you love">
			{#each stated as artist (artist.artist_mbid)}
				<li>
					<NameTag
						name={artist.artist_name}
						removing={removing === artist.artist_mbid}
						onremove={() => void remove(artist.artist_mbid)}
					>
						{#snippet picture()}
							<ArtistImage mbid={artist.artist_mbid} alt="" size="xs" rounded="full" />
						{/snippet}
					</NameTag>
				</li>
			{/each}
		</ul>
	{/if}

	{#if first}
		{@const count = Math.min(stated.length, FIRST_SET)}
		<p class="dn-stated__first">
			<span class="dn-stated__squares" aria-hidden="true">
				{#each Array.from({ length: FIRST_SET }, (_, i) => i) as i (i)}
					<i class:is-on={i < count}></i>
				{/each}
			</span>
			<span>{count} of {FIRST_SET} for a first set</span>
		</p>
	{/if}
{/snippet}

<style>
	.dn-stated {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		width: 100%;
		max-width: 30rem;
		text-align: left;
	}
	.dn-stated__note {
		margin: 0;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.dn-stated__rows {
		display: flex;
		flex-direction: column;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.dn-stated__row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.75rem;
		padding: 0.375rem 0;
		border-bottom: 1px solid var(--dn-paper-border);
	}
	.dn-stated__what {
		min-width: 0;
	}
	.dn-stated__name {
		margin: 0;
		overflow: hidden;
		font-size: 0.875rem;
		font-weight: 700;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-stated__sub {
		margin: 0;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-stated__tags {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}
	.dn-stated__tags li {
		max-width: 100%;
	}
	.dn-stated__first {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.dn-stated__squares {
		display: inline-flex;
		gap: 0.375rem;
	}
	.dn-stated__squares i {
		width: 1.75rem;
		height: 1.75rem;
		border: 2px dashed color-mix(in oklab, currentColor 40%, transparent);
		border-radius: 3px;
	}
	.dn-stated__squares i.is-on {
		border: 0;
		background: currentColor;
	}
</style>
