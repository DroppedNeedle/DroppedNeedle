<!--
	The hub's three counts as torn paper tickets, each a link to its list.
	The numbers count up and the tickets tilt toward the pointer, both off
	under reduced motion.
-->
<script lang="ts">
	import type { BrowseHeroCard } from '$lib/features/remote-library/queries/types';
	import { Disc3, Music, Users } from 'lucide-svelte';
	import { Tween } from 'svelte/motion';
	import { cubicOut } from 'svelte/easing';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		cards: BrowseHeroCard[];
	}

	let { cards }: Props = $props();

	const reducedMotion =
		typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	const iconMap = { disc: Disc3, users: Users, music: Music } as const;

	// svelte-ignore state_referenced_locally
	const counters = cards.map(
		() => new Tween(0, { duration: reducedMotion ? 0 : 1200, easing: cubicOut })
	);

	$effect(() => {
		cards.forEach((card, i) => {
			if (card.value !== null) counters[i]?.set(card.value);
		});
	});

	// svelte-ignore state_referenced_locally
	let tilts = $state<string[]>(cards.map(() => ''));

	function tilt(e: PointerEvent, i: number) {
		if (reducedMotion || e.pointerType !== 'mouse') return;
		const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
		const x = (e.clientX - rect.left) / rect.width - 0.5;
		const y = (e.clientY - rect.top) / rect.height - 0.5;
		tilts[i] = `rotateX(${(-y * 6).toFixed(1)}deg) rotateY(${(x * 8).toFixed(1)}deg)`;
	}

	function formatCount(value: number): string {
		if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
		return Math.round(value).toLocaleString();
	}
</script>

<div class="hub-counts-wrap">
	<div class="hub-counts">
		{#each cards as card, i (card.label)}
			{@const Icon = iconMap[card.icon]}
			<div class="hub-counts__tilt" style:transform={tilts[i] || undefined}>
				<Card
					variant="torn"
					padding="none"
					href={card.href}
					class="hub-count-card"
					onpointermove={(e: PointerEvent) => tilt(e, i)}
					onpointerleave={() => (tilts[i] = '')}
				>
					<span class="hub-count-card__label">
						<Icon class="hub-count-card__icon" aria-hidden="true" />{card.label}
					</span>
					{#if card.value !== null}
						<span class="hub-count-card__value">{formatCount(counters[i]?.current ?? 0)}</span>
					{:else}
						<span class="hub-count-card__value hub-count-card__value--wait" aria-hidden="true"
						></span>
					{/if}
					{#if card.subtitle}
						<span class="hub-count-card__sub">{card.subtitle}</span>
					{/if}
				</Card>
			</div>
		{/each}
	</div>
</div>

<style>
	.hub-counts-wrap {
		height: 100%;
		container: hub-counts / inline-size;
	}
	.hub-counts {
		display: grid;
		height: 100%;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 1rem;
		perspective: 900px;
	}

	.hub-counts__tilt {
		transition: transform 0.45s var(--ease-spring);
		transform-style: preserve-3d;
	}
	.hub-counts__tilt:nth-child(1) {
		rotate: -0.6deg;
	}
	.hub-counts__tilt:nth-child(3) {
		rotate: 0.5deg;
	}

	.hub-counts :global(.hub-count-card) {
		height: 100%;
	}
	.hub-counts :global(.hub-count-card > .card-body) {
		display: grid;
		align-content: center;
		gap: 0.125rem;
		padding: 1.375rem 1.25rem 1.25rem;
	}

	.hub-count-card__label {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.hub-count-card__label :global(.hub-count-card__icon) {
		width: 1.5rem;
		height: 1.5rem;
		flex: none;
	}

	.hub-count-card__value {
		font-family: var(--font-display);
		font-size: 3.5rem;
		line-height: 1.05;
		letter-spacing: 0.01em;
		font-variant-numeric: tabular-nums;
		font-synthesis: none;
		white-space: nowrap;
	}
	.hub-count-card__value--wait {
		width: 7rem;
		height: 3rem;
		margin-block: 0.15rem;
		border-radius: 0.25rem;
		background: color-mix(in oklab, var(--dn-paper-content) 12%, transparent);
	}

	.hub-count-card__sub {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
	}

	@container hub-counts (max-width: 36rem) {
		.hub-count-card__value {
			font-size: 2.5rem;
		}
		.hub-count-card__label {
			font-size: 0.8125rem;
		}
	}

	@container hub-counts (max-width: 30rem) {
		.hub-counts {
			grid-template-columns: 1fr;
			gap: 0.625rem;
		}
		.hub-counts__tilt:nth-child(n) {
			rotate: none;
		}
		.hub-counts :global(.hub-count-card > .card-body) {
			grid-template-columns: 1fr auto;
			align-items: center;
			column-gap: 1rem;
			padding: 1rem 1.25rem;
		}
		.hub-count-card__label {
			font-size: 0.8125rem;
		}
		.hub-count-card__value {
			grid-column: 2;
			grid-row: 1 / 3;
			font-size: 2.5rem;
		}
		.hub-count-card__sub {
			font-size: 0.6875rem;
		}
	}
</style>
