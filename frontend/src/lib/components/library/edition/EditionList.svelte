<script module lang="ts">
	export interface EditionRow {
		/** Unique within the list. */
		key: string;
		/** The release to compare; null for a row that can't be chosen (a custom edition). */
		release: string | null;
		name: string;
		facts: string;
		fit: string | null;
		current: boolean;
		bestGuess: boolean;
	}

	export interface EditionGroup {
		id: string;
		label: string;
		rows: EditionRow[];
	}
</script>

<script lang="ts">
	import TextField from '$lib/ui/TextField.svelte';
	import { coverPath } from '$lib/api/covers';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';

	interface Props {
		groups: EditionGroup[];
		selected: string | null;
		filter: string;
		showFilter: boolean;
		onselect: (release: string) => void;
		/** The badge on the current row. */
		currentLabel?: string;
	}

	let {
		groups,
		selected,
		filter = $bindable(''),
		showFilter,
		onselect,
		currentLabel = 'Yours now'
	}: Props = $props();
</script>

<div class="flex flex-col gap-4">
	{#if showFilter}
		<TextField
			label="Filter editions"
			type="search"
			placeholder="Year, country, CD, vinyl"
			bind:value={filter}
		/>
	{/if}

	{#each groups as group (group.id)}
		{#if group.rows.length}
			<section aria-labelledby="edition-group-{group.id}">
				<h3
					id="edition-group-{group.id}"
					class="mb-1.5 text-xs font-semibold tracking-wide text-base-content/55 uppercase"
				>
					{group.label}
				</h3>
				<ul class="flex flex-col gap-1">
					{#each group.rows as row (row.key)}
						<li>
							{#if row.release}
								{@const release = row.release}
								<button
									type="button"
									class="flex min-h-16 w-full items-center gap-3 rounded-box border px-3 py-2 text-left transition-colors focus-visible:outline-2 focus-visible:outline-primary motion-reduce:transition-none {selected ===
									release
										? 'border-primary bg-primary/10'
										: 'border-base-content/10 hover:border-primary/40'}"
									aria-pressed={selected === release}
									onclick={() => onselect(release)}
								>
									{@render rowBody(row)}
								</button>
							{:else}
								<div
									class="flex min-h-16 w-full items-center gap-3 rounded-box border border-base-content/10 px-3 py-2"
								>
									{@render rowBody(row)}
								</div>
							{/if}
						</li>
					{/each}
				</ul>
			</section>
		{/if}
	{/each}
</div>

{#snippet rowBody(row: EditionRow)}
	{#if row.release}
		<AlbumImage
			customUrl={coverPath('release', row.release, 250)}
			alt=""
			size="full"
			className="h-10 w-10 shrink-0 rounded-md"
			retryOnError={false}
		/>
	{:else}
		<div class="relative h-10 w-10 shrink-0 overflow-hidden rounded-md" aria-hidden="true">
			<CoverPlaceholder name={row.name} />
		</div>
	{/if}
	<span class="min-w-0 flex-1">
		<span class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
			<span class="font-medium">{row.name}</span>
			{#if row.current}<span class="badge badge-primary badge-xs">{currentLabel}</span>{/if}
			{#if row.bestGuess}<span class="badge badge-warning badge-xs">Best guess</span>{/if}
		</span>
		{#if row.facts}<span class="block text-xs text-base-content/60">{row.facts}</span>{/if}
		{#if row.fit}<span class="block text-xs text-base-content/75">{row.fit}</span>{/if}
	</span>
{/snippet}
