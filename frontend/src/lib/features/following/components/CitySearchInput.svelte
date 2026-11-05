<!--
	The city search: a dark pill with an acid edge, and the matches in a
	list under it. Picking a match hands it to `onpick` and clears the box.
-->
<script lang="ts">
	import { MapPin, Search } from 'lucide-svelte';
	import Input from '$lib/ui/Input.svelte';
	import { getCitySearchQuery } from '$lib/features/following/queries/FollowQueries.svelte';
	import type { CitySearchResult } from '$lib/features/following/queries/types';

	interface Props {
		onpick: (city: CitySearchResult) => void;
		placeholder?: string;
		autofocus?: boolean;
		/** Layout only: width, flex. */
		className?: string;
	}

	let {
		onpick,
		placeholder = 'Search a city…',
		autofocus = false,
		className = ''
	}: Props = $props();

	let input = $state('');
	let debounced = $state('');
	let open = $state(false);
	let timer: ReturnType<typeof setTimeout> | undefined;

	function oninput() {
		clearTimeout(timer);
		timer = setTimeout(() => {
			debounced = input;
			open = true;
		}, 300);
	}

	const query = getCitySearchQuery(() => debounced);
	const results = $derived(query.data?.items ?? []);
	const searching = $derived(query.isFetching);
	const failed = $derived(query.isError);

	function pick(city: CitySearchResult) {
		onpick(city);
		input = '';
		debounced = '';
		open = false;
	}

	function subtitle(city: CitySearchResult): string {
		return [city.region, city.country].filter(Boolean).join(', ');
	}
</script>

<div class="dn-city-search {className}">
	<div class="dn-city-search__pill">
		<Search class="size-5 shrink-0" aria-hidden="true" />
		<Input
			label="Search a city"
			borderless
			class="dn-city-search__input"
			{placeholder}
			{autofocus}
			bind:value={input}
			{oninput}
			onfocus={() => (open = debounced.trim().length >= 2)}
		/>
		{#if searching}
			<span class="loading loading-spinner loading-xs shrink-0" aria-hidden="true"></span>
		{/if}
	</div>

	{#if open && debounced.trim().length >= 2}
		<ul class="dn-city-search__list">
			{#if failed}
				<li class="px-3 py-2 text-sm text-error">
					City search is unavailable right now - try again in a moment.
				</li>
			{:else if results.length === 0 && !searching}
				<li class="px-3 py-2 text-sm text-muted">No cities match "{debounced}".</li>
			{:else}
				{#each results as city (city.name + city.latitude + city.longitude)}
					<li>
						<button type="button" class="dn-city-search__match" onclick={() => pick(city)}>
							<MapPin class="size-4 shrink-0 text-title" aria-hidden="true" />
							<span class="truncate">
								<span class="font-semibold">{city.name}</span>
								{#if subtitle(city)}
									<span class="text-sm text-muted"> · {subtitle(city)}</span>
								{/if}
							</span>
						</button>
					</li>
				{/each}
			{/if}
		</ul>
	{/if}
</div>

<style>
	.dn-city-search {
		position: relative;
	}

	/* the same dark pill on the page and on a paper card */
	.dn-city-search__pill {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		height: 3.25rem;
		padding-inline: 1.25rem;
		border: 2px solid var(--dn-action);
		border-radius: 9999px;
		background-color: var(--dn-paper-content);
		color: var(--dn-paper);
		transition: box-shadow 150ms;
	}
	:global([data-theme='light']) .dn-city-search__pill {
		border-color: var(--dn-action-edge);
		box-shadow: inset 0 0 0 2px var(--dn-action);
	}
	.dn-city-search__pill:focus-within {
		outline: 2px solid var(--dn-focus);
		outline-offset: 3px;
	}
	.dn-city-search__pill :global(.dn-city-search__input) {
		min-width: 0;
		font-size: 1rem;
		color: var(--dn-paper);
	}
	.dn-city-search__pill :global(.dn-city-search__input::placeholder) {
		color: color-mix(in oklab, var(--dn-paper) 72%, var(--dn-paper-content));
		opacity: 1;
	}

	.dn-city-search__list {
		position: absolute;
		z-index: 20;
		inset-inline: 0;
		top: calc(100% + 0.375rem);
		max-height: 16rem;
		overflow: auto;
		padding: 0.25rem;
		border: 1px solid var(--dn-edge);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
		color: var(--color-base-content);
		text-align: left;
		box-shadow: 0 18px 32px -16px rgb(0 0 0 / 0.55);
	}
	.dn-city-search__match {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 0.625rem;
		padding: 0.5rem 0.75rem;
		border-radius: calc(var(--radius-box) - 0.25rem);
		text-align: left;
	}
	.dn-city-search__match:hover,
	.dn-city-search__match:focus-visible {
		background-color: var(--color-neutral);
	}
</style>
