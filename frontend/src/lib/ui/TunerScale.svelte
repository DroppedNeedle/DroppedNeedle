<!--
	A ruled glass strip, "Closest" at its left and "Further" at its right,
	with stations set along it by their distance, from 0 at the left to `max`
	at the right. Each station is a small round cover and a button; they come
	in distance order, nearest first, and stations close together take
	turns on two lanes so none hides another. A needle in sound's plum
	stands on the tuned one (which also carries `aria-current`), travelling
	there with a slight overshoot. `cover` draws a station's face; without
	it the station gets its placeholder colours. The glass stays dark in
	every theme.
-->
<script lang="ts" module>
	export interface TunerStation {
		id: string;
		/** Its distance, 0 for the same sound. */
		distance: number;
		/** The button's name: "The Ash by Julie Christmas, very close". */
		name: string;
	}
</script>

<script lang="ts" generics="S extends TunerStation">
	import type { Snippet } from 'svelte';
	import CoverPlaceholder from './CoverPlaceholder.svelte';

	interface Props {
		/** The strip's name, for the group of stations. */
		label: string;
		stations: readonly S[];
		/** The tuned station's id. */
		tuned: string | null;
		ontune: (id: string) => void;
		/** The distance at the strip's right end. */
		max: number;
		cover?: Snippet<[S]>;
		/** Layout only: margin. */
		class?: string;
	}

	let { label, stations, tuned, ontune, max, cover, class: className = '' }: Props = $props();

	/** Stations nearer than this share of the strip go on alternate lanes. */
	const CROWDED = 0.06;

	const placed = $derived.by(() => {
		const sorted = [...stations].sort((a, b) => a.distance - b.distance);
		const lastAt = [-Infinity, -Infinity];
		return sorted.map((station) => {
			const at = Math.min(1, Math.max(0, station.distance / Math.max(max, Number.EPSILON)));
			// The top lane when it has room, else the bottom one, else the
			// lane whose last station is further back.
			const lane =
				at - lastAt[0] >= CROWDED ? 0 : at - lastAt[1] >= CROWDED || lastAt[1] < lastAt[0] ? 1 : 0;
			lastAt[lane] = at;
			return { station, at, lane };
		});
	});
	const needleAt = $derived(placed.find((p) => p.station.id === tuned)?.at ?? null);
</script>

<div class="dn-tuner {className}">
	<div class="dn-tuner__rule" aria-hidden="true"></div>
	<div class="dn-tuner__stations" role="group" aria-label={label}>
		{#each placed as { station, at, lane } (station.id)}
			<button
				type="button"
				class="dn-tuner__station"
				class:is-tuned={station.id === tuned}
				style:--at={at}
				style:--lane={lane}
				aria-label={station.name}
				aria-current={station.id === tuned ? 'true' : undefined}
				onclick={() => ontune(station.id)}
			>
				<span class="dn-tuner__face">
					{#if cover}
						{@render cover(station)}
					{:else}
						<CoverPlaceholder name={station.name} />
					{/if}
				</span>
			</button>
		{/each}
	</div>
	{#if needleAt !== null}
		<span class="dn-tuner__needle" style:--at={needleAt} aria-hidden="true"></span>
	{/if}
	<div class="dn-tuner__ends" aria-hidden="true">
		<span>Closest</span>
		<span>Further</span>
	</div>
</div>

<style>
	.dn-tuner {
		--tuner-inset: 1.25rem;
		--tuner-face: 1.75rem;
		position: relative;
		padding: 0.625rem var(--tuner-inset) 0.5rem;
		border-radius: 0.75rem;
		background: var(--dn-vinyl);
		box-shadow:
			inset 0 2px 8px rgb(0 0 0 / 0.7),
			inset 0 0 0 1px rgb(0 0 0 / 0.6),
			0 1px 0 rgb(255 255 255 / 0.06);
		color: var(--dn-arm);
	}
	.dn-tuner__rule {
		height: 0.875rem;
		background-image:
			repeating-linear-gradient(to right, rgb(255 255 255 / 0.55) 0 1px, transparent 1px 10%),
			repeating-linear-gradient(to right, rgb(255 255 255 / 0.25) 0 1px, transparent 1px 2.5%);
		background-size:
			100% 100%,
			100% 55%;
		background-repeat: no-repeat;
		border-right: 1px solid rgb(255 255 255 / 0.55);
	}
	.dn-tuner__stations {
		position: relative;
		height: calc(var(--tuner-face) * 2 + 0.75rem);
		margin-top: 0.375rem;
	}
	.dn-tuner__station {
		position: absolute;
		top: calc(var(--lane) * (var(--tuner-face) + 0.5rem));
		left: calc(var(--at) * 100%);
		translate: -50% 0;
		display: grid;
		place-items: center;
		width: 2.75rem;
		height: 2.75rem;
		margin-top: -0.5rem;
		padding: 0;
		border: 0;
		background: none;
		cursor: pointer;
		border-radius: 9999px;
	}
	.dn-tuner__station:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: -2px;
	}
	.dn-tuner__face {
		position: relative;
		overflow: hidden;
		width: var(--tuner-face);
		height: var(--tuner-face);
		border-radius: 9999px;
		box-shadow:
			0 0 0 1.5px rgb(255 255 255 / 0.25),
			0 2px 4px rgb(0 0 0 / 0.6);
		transition: scale 0.2s ease;
	}
	.dn-tuner__station:hover .dn-tuner__face,
	.dn-tuner__station.is-tuned .dn-tuner__face {
		scale: 1.25;
	}
	.dn-tuner__station.is-tuned .dn-tuner__face {
		box-shadow:
			0 0 0 2px var(--dn-sound),
			0 2px 6px rgb(0 0 0 / 0.6);
	}
	.dn-tuner__needle {
		position: absolute;
		top: 0.25rem;
		bottom: 1.5rem;
		left: calc(var(--tuner-inset) + var(--at) * (100% - 2 * var(--tuner-inset)));
		width: 2px;
		margin-left: -1px;
		border-radius: 1px;
		background: var(--dn-sound);
		box-shadow: 0 0 0.375rem color-mix(in oklab, var(--dn-sound) 70%, transparent);
		pointer-events: none;
		transition: left 0.52s cubic-bezier(0.34, 1.56, 0.64, 1);
	}
	.dn-tuner__needle::before {
		content: '';
		position: absolute;
		top: -0.25rem;
		left: 50%;
		translate: -50% 0;
		border: 0.3125rem solid transparent;
		border-top-color: var(--dn-sound);
		border-bottom: 0;
	}
	.dn-tuner__ends {
		display: flex;
		justify-content: space-between;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 600;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		color: color-mix(in oklab, var(--dn-arm) 75%, transparent);
	}

	@media (forced-colors: active) {
		.dn-tuner__needle {
			background: Highlight;
		}
		.dn-tuner__station.is-tuned .dn-tuner__face {
			outline: 2px solid Highlight;
		}
	}
</style>
