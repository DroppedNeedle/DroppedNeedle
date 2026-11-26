<!--
	An artist's shelf with a hole in it: the albums held, small square covers
	on a rule in date order with their years, and a dashed slot where the
	missing album's year falls; one with no known year goes at the end. With
	more albums than `most`, the ones nearest the slot are drawn. It is one
	image named by `label` ("Their albums on your shelves, with a space
	where Katy Lied would go").
-->
<script lang="ts" module>
	export interface GapShelfItem {
		key: string;
		/** Its year, when known. */
		year?: number | null;
	}
</script>

<script lang="ts" generics="T extends GapShelfItem">
	import type { Snippet } from 'svelte';
	import { Plus } from 'lucide-svelte';

	interface Props {
		/** The albums held. */
		items: T[];
		/** The missing album's year, when known. */
		year?: number | null;
		/** The sentence the drawing says. */
		label: string;
		cover: Snippet<[T]>;
		/** The most covers drawn. */
		most?: number;
		/** Layout only: margin, width. */
		class?: string;
	}

	let { items, year = null, label, cover, most = 5, class: className = '' }: Props = $props();

	type Place = { kind: 'held'; item: T } | { kind: 'gap' };

	const known = (value: number | null | undefined): value is number =>
		typeof value === 'number' && Number.isFinite(value);

	// Held albums oldest first, those with no year last, and the slot after
	// every album of its year or before.
	const places = $derived.by((): Place[] => {
		const held = items
			.map((item, at) => ({ item, at }))
			.sort((a, b) => {
				const [x, y] = [a.item.year, b.item.year];
				if (known(x) && known(y)) return x - y || a.at - b.at;
				if (known(x)) return -1;
				if (known(y)) return 1;
				return a.at - b.at;
			})
			.map(({ item }): Place => ({ kind: 'held', item }));
		const slot = known(year)
			? held.filter(
					(place) => place.kind === 'held' && known(place.item.year) && place.item.year <= year
				).length
			: held.length;
		const all: Place[] = [...held.slice(0, slot), { kind: 'gap' }, ...held.slice(slot)];
		const room = Math.max(1, Math.round(most));
		if (held.length <= room) return all;
		const first = Math.min(Math.max(0, slot - Math.floor(room / 2)), held.length - room);
		return all.slice(first, first + room + 1);
	});
</script>

<div class="dn-gapshelf {className}" role="img" aria-label={label}>
	{#each places as place (place.kind === 'gap' ? 'gap' : place.item.key)}
		{#if place.kind === 'gap'}
			<span class="dn-gapshelf__album dn-gapshelf__album--gap" aria-hidden="true">
				<span class="dn-gapshelf__cover"><Plus class="h-5 w-5" /></span>
				<span class="dn-gapshelf__year">{known(year) ? year : ''}</span>
			</span>
		{:else}
			<span class="dn-gapshelf__album" aria-hidden="true">
				<span class="dn-gapshelf__cover">{@render cover(place.item)}</span>
				<span class="dn-gapshelf__year">{known(place.item.year) ? place.item.year : ''}</span>
			</span>
		{/if}
	{/each}
</div>

<style>
	.dn-gapshelf {
		display: flex;
		align-items: flex-end;
		gap: 0.4375rem;
		border-bottom: 4px solid var(--color-neutral);
	}

	.dn-gapshelf__album {
		display: flex;
		flex-direction: column;
		align-items: center;
		width: 3.75rem;
		min-width: 0;
		flex-shrink: 0;
	}

	.dn-gapshelf__cover {
		display: block;
		width: 100%;
		aspect-ratio: 1;
		overflow: hidden;
		box-shadow: 0 6px 12px -8px rgb(0 0 0 / 0.9);
	}
	.dn-gapshelf__album--gap .dn-gapshelf__cover {
		display: grid;
		place-items: center;
		border: 2px dashed var(--dn-action-edge);
		color: var(--dn-title);
		box-shadow: none;
	}

	.dn-gapshelf__year {
		min-height: 1.5rem;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 500;
		line-height: 1.5rem;
		letter-spacing: 0.1em;
		color: var(--dn-muted);
	}
	.dn-gapshelf__album--gap .dn-gapshelf__year {
		font-weight: 700;
		color: var(--color-base-content);
	}

	@media (max-width: 639px) {
		.dn-gapshelf__album {
			width: 2.75rem;
		}
	}

	@media (forced-colors: active) {
		.dn-gapshelf__album--gap .dn-gapshelf__cover {
			border-color: CanvasText;
		}
	}
</style>
