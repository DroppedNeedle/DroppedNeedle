<!--
	The city bar: a chip for each saved city, which opens its menu (the
	radius, or remove it), and "Add city", which swaps in the city search.
	Every change saves at once.
-->
<script lang="ts">
	import { Check, ChevronDown, MapPin, Plus, X } from 'lucide-svelte';
	import CitySearchInput from '$lib/features/following/components/CitySearchInput.svelte';
	import { createReplaceEventCitiesMutation } from '$lib/features/following/queries/FollowMutations.svelte';
	import type { CitySearchResult, EventCity } from '$lib/features/following/queries/types';
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

	function radiusMiles(radiusKm: number): number {
		return Math.round(radiusKm / KM_PER_MILE);
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

<div class="dn-cities">
	<span class="dn-cities__label">Your cities</span>
	<div class="dn-cities__row">
		{#each cities as city, index (city.latitude + ':' + city.longitude)}
			{@const miles = radiusMiles(city.radius_km)}
			<div class="dropdown dropdown-bottom">
				<button
					type="button"
					tabindex="0"
					class="dn-cities__chip"
					title="Within {miles} mi of {city.city_name}"
				>
					<MapPin class="size-4 shrink-0 text-title" aria-hidden="true" />
					<span class="font-semibold">{city.city_name}</span>
					<span class="dn-cities__radius">{miles} mi</span>
					<ChevronDown class="dn-cities__caret size-4 shrink-0" aria-hidden="true" />
				</button>
				<ul class="dropdown-content dn-cities__menu">
					{#each RADIUS_PRESETS_MI as radiusMi (radiusMi)}
						{@const current = miles === radiusMi}
						<li>
							<button
								type="button"
								class="dn-cities__item"
								class:font-bold={current}
								onclick={() => setRadius(index, radiusMi)}
							>
								<span class="dn-cities__tick">
									{#if current}<Check class="size-4" aria-hidden="true" />{/if}
								</span>
								within {radiusMi} mi
							</button>
						</li>
					{/each}
					<li class="dn-cities__rule" role="separator"></li>
					<li>
						<button
							type="button"
							class="dn-cities__item text-error"
							onclick={() => removeCity(index)}
						>
							<span class="dn-cities__tick"><X class="size-4" aria-hidden="true" /></span>
							Remove city
						</button>
					</li>
				</ul>
			</div>
		{/each}

		{#if adding}
			<CitySearchInput onpick={addCity} autofocus className="dn-cities__search" />
			<Button variant="caps" size="sm" onclick={() => (adding = false)}>Cancel</Button>
		{:else}
			<button type="button" class="dn-cities__chip dn-cities__add" onclick={() => (adding = true)}>
				<Plus class="size-4 shrink-0" aria-hidden="true" /> Add city
			</button>
		{/if}
	</div>
</div>

<style>
	.dn-cities {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem 1.25rem;
	}
	.dn-cities__label {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-cities__row {
		display: flex;
		min-width: 0;
		flex: 1;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.625rem;
	}

	.dn-cities__chip {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		height: 2.75rem;
		padding-inline: 1rem 0.875rem;
		border: 1px solid var(--dn-edge);
		border-radius: 9999px;
		background-color: var(--color-base-200);
		white-space: nowrap;
		transition:
			border-color 150ms,
			background-color 150ms;
	}
	.dn-cities__chip:hover {
		border-color: var(--dn-title);
	}
	.dropdown:focus-within .dn-cities__chip {
		border-color: var(--dn-title);
		background-color: var(--dn-selected);
	}
	.dn-cities__radius {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--dn-title);
	}
	.dn-cities__chip :global(.dn-cities__caret) {
		color: var(--dn-muted);
		transition: rotate 150ms;
	}
	.dropdown:focus-within .dn-cities__chip :global(.dn-cities__caret) {
		rotate: 180deg;
	}

	.dn-cities__add {
		padding-inline: 1rem 1.125rem;
		border-style: dashed;
		background-color: transparent;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-cities__add:hover {
		color: var(--dn-title);
	}

	.dn-cities__row :global(.dn-cities__search) {
		width: min(20rem, 100%);
	}

	.dn-cities__menu {
		z-index: 20;
		width: 13rem;
		margin-top: 0.375rem;
		padding: 0.375rem;
		border: 1px solid var(--dn-edge);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
		box-shadow: 0 18px 32px -16px rgb(0 0 0 / 0.55);
	}
	.dn-cities__item {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 0.5rem;
		padding: 0.5rem 0.625rem;
		border-radius: calc(var(--radius-box) - 0.25rem);
		font-size: 0.9375rem;
		text-align: left;
	}
	.dn-cities__item:hover,
	.dn-cities__item:focus-visible {
		background-color: var(--color-neutral);
	}
	.dn-cities__tick {
		display: grid;
		width: 1rem;
		flex: none;
		place-items: center;
		color: var(--dn-title);
	}
	.text-error .dn-cities__tick {
		color: inherit;
	}
	.dn-cities__rule {
		height: 1px;
		margin: 0.375rem 0.25rem;
		background-color: var(--dn-line);
	}

	@media (max-width: 639px) {
		.dn-cities__label {
			flex-basis: 100%;
		}
		.dn-cities__chip {
			height: 2.5rem;
			gap: 0.375rem;
			padding-inline: 0.875rem 0.625rem;
			font-size: 0.9375rem;
		}
		.dn-cities__chip:not(.dn-cities__add) :global(svg:first-child) {
			display: none;
		}
		.dn-cities__radius {
			font-size: 0.75rem;
			letter-spacing: 0.04em;
		}
		.dn-cities__row :global(.dn-cities__search) {
			width: 100%;
		}
	}
</style>
