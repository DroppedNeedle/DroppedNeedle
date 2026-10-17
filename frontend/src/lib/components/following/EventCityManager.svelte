<script lang="ts">
	import { Plus, X } from 'lucide-svelte';
	import CitySearchInput from '$lib/components/following/CitySearchInput.svelte';
	import { createReplaceEventCitiesMutation } from '$lib/queries/following/FollowMutations.svelte';
	import type { CitySearchResult, EventCity } from '$lib/queries/following/types';
	import { KM_PER_MILE } from '$lib/constants';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		cities: EventCity[];
	}

	let { cities }: Props = $props();

	// radius presets: shown in miles, stored in km
	const RADIUS_PRESETS_MI = [10, 20, 50, 100];
	const DEFAULT_RADIUS_KM = 32; // 20 mi

	let adding = $state(false);
	const replace = createReplaceEventCitiesMutation();

	function radiusLabel(radiusKm: number): string {
		return `${Math.round(radiusKm / KM_PER_MILE)} mi`;
	}

	function addCity(picked: CitySearchResult) {
		adding = false;
		if (cities.some((c) => c.latitude === picked.latitude && c.longitude === picked.longitude)) {
			return; // already saved
		}
		replace.mutate([
			...cities,
			{
				city_name: picked.name,
				latitude: picked.latitude,
				longitude: picked.longitude,
				radius_km: DEFAULT_RADIUS_KM,
				country_code: picked.country_code
			}
		]);
	}

	function removeCity(index: number) {
		replace.mutate(cities.filter((_, i) => i !== index));
	}

	function setRadius(index: number, radiusMi: number) {
		replace.mutate(
			cities.map((c, i) =>
				i === index ? { ...c, radius_km: Math.round(radiusMi * KM_PER_MILE) } : c
			)
		);
		// close the DaisyUI dropdown by dropping focus from it
		(document.activeElement as HTMLElement | null)?.blur();
	}
</script>

<div class="flex flex-wrap items-center gap-2">
	<span class="text-xs font-semibold uppercase tracking-wider text-base-content/50">
		Your cities
	</span>
	{#each cities as city, index (city.latitude + ':' + city.longitude)}
		<div class="dropdown dropdown-bottom">
			<button
				type="button"
				tabindex="0"
				class="badge badge-lg gap-1.5 border-base-300 bg-base-200 py-3"
				title="Within {radiusLabel(city.radius_km)} of {city.city_name}"
			>
				<span class="font-medium">{city.city_name}</span>
				<span class="text-xs text-base-content/50">{radiusLabel(city.radius_km)}</span>
			</button>
			<ul
				class="menu dropdown-content z-20 mt-1 w-44 rounded-xl border border-base-300 bg-base-200 p-1 shadow-lg"
			>
				{#each RADIUS_PRESETS_MI as radiusMi (radiusMi)}
					<li>
						<button
							type="button"
							class:font-semibold={Math.round(city.radius_km / KM_PER_MILE) === radiusMi}
							onclick={() => setRadius(index, radiusMi)}
						>
							within {radiusMi} mi
						</button>
					</li>
				{/each}
				<li>
					<button type="button" class="text-error" onclick={() => removeCity(index)}>
						<X class="h-3.5 w-3.5" aria-hidden="true" /> Remove city
					</button>
				</li>
			</ul>
		</div>
	{/each}

	{#if adding}
		<CitySearchInput onpick={addCity} autofocus className="w-64" />
		<Button variant="ghost" size="xs" pill onclick={() => (adding = false)}>Cancel</Button>
	{:else}
		<button
			type="button"
			class="badge badge-lg gap-1 border-dashed border-base-300 bg-transparent py-3 text-base-content/60 hover:border-primary hover:text-primary"
			onclick={() => (adding = true)}
		>
			<Plus class="h-3.5 w-3.5" aria-hidden="true" /> Add city
		</button>
	{/if}
</div>
