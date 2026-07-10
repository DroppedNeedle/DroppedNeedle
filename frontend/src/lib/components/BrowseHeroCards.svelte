<script lang="ts">
	import type { BrowseHeroCard } from '$lib/types';
	import { Disc3, Users, Music, ChevronRight } from 'lucide-svelte';
	import { tweened, type Tweened } from 'svelte/motion';
	import { cubicOut } from 'svelte/easing';

	interface Props {
		cards: BrowseHeroCard[];
	}

	let { cards }: Props = $props();

	const reducedMotion =
		typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	const iconMap = { disc: Disc3, users: Users, music: Music } as const;

	const colorClasses: Record<
		BrowseHeroCard['colorScheme'],
		{ gradient: string; border: string; glow: string; text: string }
	> = {
		primary: {
			gradient: 'from-primary/15 via-primary/5 to-transparent',
			border: 'border-primary/15',
			glow: 'shadow-[0_8px_32px_oklch(from_var(--color-primary)_l_c_h_/_0.12)]',
			text: 'text-primary'
		},
		secondary: {
			gradient: 'from-secondary/15 via-secondary/5 to-transparent',
			border: 'border-secondary/15',
			glow: 'shadow-[0_8px_32px_oklch(from_var(--color-secondary)_l_c_h_/_0.12)]',
			text: 'text-secondary'
		},
		accent: {
			gradient: 'from-accent/15 via-accent/5 to-transparent',
			border: 'border-accent/15',
			glow: 'shadow-[0_8px_32px_oklch(from_var(--color-accent)_l_c_h_/_0.12)]',
			text: 'text-accent'
		}
	};

	const glowHover: Record<BrowseHeroCard['colorScheme'], string> = {
		primary: 'shadow-[0_12px_48px_oklch(from_var(--color-primary)_l_c_h_/_0.25)]',
		secondary: 'shadow-[0_12px_48px_oklch(from_var(--color-secondary)_l_c_h_/_0.25)]',
		accent: 'shadow-[0_12px_48px_oklch(from_var(--color-accent)_l_c_h_/_0.25)]'
	};

	let hoveredIndex = $state<number | null>(null);
	let cardEls: HTMLAnchorElement[] = [];
	// svelte-ignore state_referenced_locally
	let tiltStyles: string[] = $state(Array(cards.length).fill(''));
	// svelte-ignore state_referenced_locally
	let specularStyles: string[] = $state(Array(cards.length).fill(''));

	const tweenDuration = reducedMotion ? 0 : 1200;
	// svelte-ignore state_referenced_locally
	const counters: Tweened<number>[] = Array.from({ length: cards.length }, () =>
		tweened(0, { duration: tweenDuration, easing: cubicOut })
	);
	// svelte-ignore state_referenced_locally
	let counterValues: number[] = $state(Array(cards.length).fill(0));

	$effect(() => {
		const unsubs = counters.map((c, i) =>
			c.subscribe((v) => {
				counterValues[i] = v;
			})
		);
		return () => unsubs.forEach((u) => u());
	});

	$effect(() => {
		cards.forEach((card, i) => {
			if (card.value !== null && card.value !== undefined) counters[i].set(card.value);
		});
	});

	function handlePointerMove(e: PointerEvent, i: number) {
		if (reducedMotion) return;
		const el = cardEls[i];
		if (!el) return;
		const rect = el.getBoundingClientRect();
		const x = e.clientX - rect.left;
		const y = e.clientY - rect.top;
		const centerX = rect.width / 2;
		const centerY = rect.height / 2;
		const rotateY = ((x - centerX) / centerX) * 4;
		const rotateX = ((centerY - y) / centerY) * 3;
		tiltStyles[i] =
			`rotateX(${rotateX.toFixed(1)}deg) rotateY(${rotateY.toFixed(1)}deg) translateZ(0)`;
		const pctX = ((x / rect.width) * 100).toFixed(0);
		const pctY = ((y / rect.height) * 100).toFixed(0);
		specularStyles[i] =
			`radial-gradient(circle at ${pctX}% ${pctY}%, rgba(255,255,255,0.08) 0%, transparent 60%)`;
	}

	function handlePointerLeave(i: number) {
		hoveredIndex = null;
		tiltStyles[i] = '';
		specularStyles[i] = '';
	}

	function formatNumber(value: number): string {
		if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
		if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
		return value.toLocaleString();
	}

	function getCounterValue(i: number): number {
		return counterValues[i];
	}
</script>

<div
	class="grid grid-cols-1 gap-5 sm:grid-cols-2 sm:gap-6 md:grid-cols-3"
	style="perspective: 1200px;"
>
	{#each cards as card, i (card.label)}
		{@const colors = colorClasses[card.colorScheme]}
		{@const Icon = iconMap[card.icon]}
		{@const isHovered = hoveredIndex === i}
		{@const isSibling = hoveredIndex !== null && hoveredIndex !== i}
		<a
			href={card.href}
			bind:this={cardEls[i]}
			class="group relative flex min-h-[160px] flex-col justify-center overflow-hidden rounded-2xl border bg-base-200/50 bg-gradient-to-br px-6 py-5 text-center backdrop-blur-sm transition-all sm:min-h-[180px] {colors.gradient} {colors.border} {isHovered
				? glowHover[card.colorScheme]
				: colors.glow} focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100"
			style="transform-style: preserve-3d; transform: {tiltStyles[i] ||
				'rotateX(0) rotateY(0)'}; transition: transform 0.5s var(--ease-spring), box-shadow 0.5s var(--ease-spring), opacity 0.3s ease, scale 0.3s var(--ease-spring); {!reducedMotion &&
			isSibling
				? 'scale: 0.97; opacity: 0.7;'
				: ''}"
			onpointermove={(e) => {
				hoveredIndex = i;
				handlePointerMove(e, i);
			}}
			onpointerleave={() => handlePointerLeave(i)}
		>
			<div
				class="pointer-events-none absolute inset-0 rounded-2xl bg-gradient-to-br from-white/[0.06] to-transparent"
			></div>

			{#if specularStyles[i]}
				<div
					class="pointer-events-none absolute inset-0 rounded-2xl"
					style="background: {specularStyles[i]};"
				></div>
			{/if}

			<div class="pointer-events-none absolute -right-2 -top-2 opacity-[0.06]">
				<Icon size={72} strokeWidth={1} />
			</div>

			{#if card.value !== null}
				<div
					class="font-display text-4xl font-bold tabular-nums tracking-tight sm:text-5xl {colors.text}"
					style="text-shadow: 0 0 30px oklch(from var(--color-{card.colorScheme}) l c h / 0.25);"
				>
					{formatNumber(Math.round(getCounterValue(i)))}
				</div>
			{:else}
				<div class="mx-auto h-10 w-24 animate-glow-pulse rounded-lg bg-base-200/40 sm:h-12"></div>
			{/if}

			<div
				class="mt-3 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/60"
			>
				{card.label}
			</div>

			{#if card.subtitle}
				<div class="mt-1 text-xs text-base-content/40">{card.subtitle}</div>
			{/if}

			<div
				class="absolute right-4 top-1/2 -translate-y-1/2 translate-x-2 opacity-30 transition-all duration-300 group-hover:translate-x-0 group-hover:opacity-70"
			>
				<ChevronRight class="h-5 w-5 text-base-content" />
			</div>
		</a>
	{/each}
</div>
