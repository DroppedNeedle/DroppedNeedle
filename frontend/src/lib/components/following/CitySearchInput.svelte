<script lang="ts">
	import { MapPin, Search } from 'lucide-svelte';
	import { getCitySearchQuery } from '$lib/queries/following/FollowQueries.svelte';
	import type { CitySearchResult } from '$lib/queries/following/types';

	interface Props {
		onpick: (city: CitySearchResult) => void;
		placeholder?: string;
		autofocus?: boolean;
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
	import Input from '$lib/ui/Input.svelte';
</script>

<div class="relative {className}">
	<Input
		label="Search a city"
		icon={Search}
		class="w-full pr-9"
		{placeholder}
		{autofocus}
		bind:value={input}
		{oninput}
		onfocus={() => (open = debounced.trim().length >= 2)}
	/>
	{#if searching}
		<span class="loading loading-spinner loading-xs absolute right-3 top-3" aria-hidden="true"
		></span>
	{/if}

	{#if open && debounced.trim().length >= 2}
		<ul
			class="absolute z-20 mt-1 max-h-64 w-full overflow-auto rounded-xl border border-base-300 bg-base-200 p-1 shadow-lg"
		>
			{#if failed}
				<li class="px-3 py-2 text-sm text-error">
					City search is unavailable right now - try again in a moment.
				</li>
			{:else if results.length === 0 && !searching}
				<li class="px-3 py-2 text-sm text-base-content/60">No cities match "{debounced}".</li>
			{:else}
				{#each results as city (city.name + city.latitude + city.longitude)}
					<li>
						<button
							type="button"
							class="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left hover:bg-base-300"
							onclick={() => pick(city)}
						>
							<MapPin class="h-4 w-4 shrink-0 text-primary" aria-hidden="true" />
							<span class="truncate">
								<span class="font-medium">{city.name}</span>
								{#if subtitle(city)}
									<span class="text-sm text-base-content/60"> · {subtitle(city)}</span>
								{/if}
							</span>
						</button>
					</li>
				{/each}
			{/if}
		</ul>
	{/if}
</div>
