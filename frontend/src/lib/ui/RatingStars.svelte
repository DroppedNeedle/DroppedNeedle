<!--
	Five stars for the caller's own rating. Clicking a star rates that many;
	clicking the star given clears the rating. Hover or focus previews the
	fill. `label` names what is rated, so it is required. `min` makes the
	stars below it a fixed part of every pick (a floor of 2 to 5 stars).
-->
<script lang="ts">
	import { Star } from 'lucide-svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	interface Props extends Omit<HTMLAttributes<HTMLDivElement>, 'onchange' | 'children'> {
		/** 0 to 5; 0 is not rated. */
		value: number;
		/** What is rated, e.g. the track title. */
		label: string;
		/** 1 to 5, or 0 when the given star is clicked again. */
		onchange: (rating: number) => void;
		/** 'sm' on rows, 'md' in headers. */
		size?: 'sm' | 'md';
		/**
		 * For a list row: empty stars draw in the line colour until the row
		 * sets `--dn-quiet-ink` (on hover or focus within) to bring them up.
		 */
		quiet?: boolean;
		/** The fewest stars a pick can be; the ones below it fill with the pick and take no click. */
		min?: number;
		/** The group's accessible name, in place of "<label>: 3 of 5 stars". */
		groupLabel?: string;
	}

	let {
		value,
		label,
		onchange,
		size = 'sm',
		quiet = false,
		min = 1,
		groupLabel: namedGroup = undefined,
		class: className = '',
		...rest
	}: Props = $props();

	const emptyInk = $derived(
		quiet ? 'text-[color:var(--dn-quiet-ink,var(--dn-line))]' : 'text-edge'
	);

	const STARS = [1, 2, 3, 4, 5];
	// Whole class strings, so Tailwind's scan finds every one.
	const sizes = {
		sm: { button: 'btn-xs', icon: 'size-3.5', box: 'size-6' },
		md: { button: 'btn-sm', icon: 'size-5', box: 'size-8' }
	};

	let preview = $state(0);
	const shown = $derived(preview || value);
	const groupLabel = $derived(
		namedGroup ?? (value > 0 ? `${label}: ${value} of 5 stars` : `${label}: not rated`)
	);
</script>

<div
	role="group"
	aria-label={groupLabel}
	{...rest}
	class="inline-flex shrink-0 items-center {className}"
>
	{#each STARS as star (star)}
		{#if star < min}
			<span class="inline-grid {sizes[size].box} place-items-center" aria-hidden="true">
				<Star
					class="{sizes[size].icon} {shown > 0 ? 'fill-warning text-warning' : emptyInk}"
					aria-hidden="true"
				/>
			</span>
		{:else}
			<button
				type="button"
				class="btn btn-ghost dn-btn-ghost btn-square {sizes[size].button}"
				aria-label={star === 1 ? '1 star' : `${star} stars`}
				aria-pressed={star === value}
				title={star === value ? 'Clear rating' : undefined}
				onclick={() => onchange(star === value ? 0 : star)}
				onmouseenter={() => (preview = star)}
				onmouseleave={() => (preview = 0)}
				onfocus={() => (preview = star)}
				onblur={() => (preview = 0)}
			>
				<Star
					class="{sizes[size].icon} {star <= shown ? 'fill-warning text-warning' : emptyInk}"
					aria-hidden="true"
				/>
			</button>
		{/if}
	{/each}
</div>
