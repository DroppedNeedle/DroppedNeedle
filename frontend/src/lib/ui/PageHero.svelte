<script lang="ts">
	/*
	 * PageHero — shared masthead for top-level surfaces (Discover, Library,
	 * Playlists, …). Display-face title, quiet eyebrow, optional status line and
	 * action row; an optional accent wash keeps each surface's identity.
	 */
	import { formatLastUpdated } from '$lib/utils/formatting';
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		subtitle?: string;
		eyebrow?: string;
		loading?: boolean;
		isUpdating?: boolean;
		lastUpdated?: Date | null;
		/** CSS color used for the top radial wash (defaults to accent) */
		tint?: string;
		icon?: Snippet;
		actions?: Snippet;
	}

	let {
		title,
		subtitle = '',
		eyebrow = '',
		loading = false,
		isUpdating = false,
		lastUpdated = null,
		tint = 'var(--color-accent)',
		icon,
		actions
	}: Props = $props();
</script>

<header class="dn-page-hero" style="--dn-hero-tint: {tint}">
	<div class="dn-page-hero__wash" aria-hidden="true"></div>
	<div class="dn-page-hero__content">
		{#if eyebrow}
			<p class="dn-page-hero__eyebrow">{eyebrow}</p>
		{/if}
		<h1 class="dn-page-hero__title hero-title">
			{#if icon}<span class="dn-page-hero__icon">{@render icon()}</span>{/if}
			{title}
		</h1>
		{#if subtitle}
			<p class="dn-page-hero__sub">{subtitle}</p>
		{/if}
		{#if actions}
			<div class="mt-4 flex flex-wrap items-center gap-2.5">{@render actions()}</div>
		{/if}
		<p class="mt-3 min-h-4 text-xs text-base-content/45">
			{#if loading}
				Loading…
			{:else if isUpdating}
				<span class="inline-flex items-center gap-1.5">
					<span class="dn-page-hero__pulse"></span>
					Updating…
				</span>
			{:else if lastUpdated}
				Updated {formatLastUpdated(lastUpdated)}
			{/if}
		</p>
	</div>
</header>

<style>
	.dn-page-hero {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		padding: 2.75rem 1rem 1.5rem;
	}
	@media (min-width: 640px) {
		.dn-page-hero {
			padding: 3.25rem 1.5rem 1.75rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-page-hero {
			padding: 3.75rem 2rem 2rem;
		}
	}

	.dn-page-hero__wash {
		position: absolute;
		inset: 0;
		z-index: -1;
		background:
			radial-gradient(
				circle at 15% -40%,
				oklch(from var(--dn-hero-tint) l c h / 0.18),
				transparent 55%
			),
			radial-gradient(
				circle at 85% -60%,
				oklch(from var(--color-primary) l c h / 0.1),
				transparent 55%
			);
	}

	.dn-page-hero__eyebrow {
		margin-bottom: 0.55rem;
		font-family: var(--font-mono);
		font-size: 0.62rem;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}

	.dn-page-hero__title {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		font-family: var(--font-display);
		font-size: clamp(2rem, 4.5vw, 3.25rem);
		font-weight: 700;
		line-height: 1.05;
		letter-spacing: -0.015em;
	}

	.dn-page-hero__icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0.5rem;
		border-radius: var(--dn-radius-sm);
		background: oklch(from var(--dn-hero-tint) l c h / 0.14);
		color: oklch(from var(--dn-hero-tint) l c h);
	}

	.dn-page-hero__sub {
		margin-top: 0.5rem;
		max-width: 36rem;
		font-size: 0.95rem;
		color: oklch(from var(--color-base-content) l c h / 0.62);
	}

	.dn-page-hero__pulse {
		display: inline-block;
		width: 0.45rem;
		height: 0.45rem;
		border-radius: 999px;
		background: var(--color-primary);
		animation: dn-page-hero-pulse 1.5s ease-in-out infinite;
	}
	@keyframes dn-page-hero-pulse {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.8);
		}
		50% {
			opacity: 1;
			transform: scale(1);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.dn-page-hero__pulse {
			animation: none;
		}
	}
</style>
