<!--
	An empty state on the Following pages. `paper` is a torn cream sheet
	with a halftone record in its corner, `dark` sits on the card itself
	with a map fragment at its edge (Coming up), `band` is one row with a
	crowd printed along its left (Your artists). The button is the one way
	on, and sits above the card's own link.
-->
<script lang="ts">
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import Button from '$lib/ui/Button.svelte';
	import HalftoneArt from '$lib/ui/HalftoneArt.svelte';

	interface Props {
		look?: 'paper' | 'dark' | 'band';
		icon: KitIcon;
		title: string;
		description?: string;
		ctaLabel?: string;
		ctaHref?: string;
		/** The heading level on its page. */
		titleAs?: 'h2' | 'h3';
	}

	let {
		look = 'paper',
		icon: Icon,
		title,
		description,
		ctaLabel,
		ctaHref,
		titleAs = 'h3'
	}: Props = $props();

	const decor = $derived(look === 'dark' ? 'map' : 'crowd');
</script>

<div class="fe" data-look={look}>
	{#if look === 'paper'}
		<!-- the record is printed on the sheet, so the tear cuts it too -->
		<span class="fe__sheet" aria-hidden="true">
			<span class="fe__paper dn-card-paper dn-card-torn">
				<HalftoneArt name="record" class="fe__decor" />
			</span>
		</span>
	{:else}
		<HalftoneArt name={decor} fade={look === 'dark' ? 'right' : 'left'} class="fe__decor" />
	{/if}
	<div class="fe__stamp">
		<Icon class="size-7" strokeWidth={1.75} aria-hidden="true" />
	</div>
	{#if look === 'band'}<span class="fe__rule" aria-hidden="true"></span>{/if}
	<div class="fe__words">
		<svelte:element this={titleAs} class="fe__title">{title}</svelte:element>
		{#if description}<p class="fe__desc">{description}</p>{/if}
	</div>
	{#if ctaLabel && ctaHref}
		<Button variant="primary" size="display-sm" href={ctaHref} class="dn-follow-over fe__cta">
			{ctaLabel}
		</Button>
	{/if}
</div>

<style>
	.fe {
		--ink: var(--color-base-content);
		position: relative;
		isolation: isolate;
		display: flex;
		flex: 1;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.875rem;
		min-height: 15rem;
		padding: 2.25rem 2rem 2.5rem;
		text-align: center;
		color: var(--ink);
	}

	/* the paper is its own layer, so the tear masks only the sheet */
	.fe[data-look='paper'] {
		--ink: var(--dn-paper-content);
		--dn-title: var(--dn-paper-content);
		--fe-muted: color-mix(in oklab, var(--dn-paper-content) 76%, var(--dn-paper));
	}
	.fe__sheet {
		position: absolute;
		inset: 0;
		z-index: -1;
	}
	.fe__paper {
		position: absolute;
		inset: 0;
		background-color: var(--dn-paper);
	}
	/* in light the sheet is paper on paper: a hairline and a soft shadow
	   follow the tear, drawn outside the mask */
	:global([data-theme='light']) .fe__sheet {
		filter: drop-shadow(0 0 0.6px color-mix(in oklab, var(--dn-paper-content) 75%, transparent))
			drop-shadow(0 8px 12px color-mix(in oklab, var(--dn-paper-content) 16%, transparent));
	}
	:global([data-theme='light']) .fe__paper {
		box-shadow: none;
	}

	.fe :global(.fe__decor) {
		position: absolute;
		z-index: -1;
	}
	.fe__paper :global(.fe__decor) {
		z-index: auto;
	}
	.fe[data-look='paper'] :global(.fe__decor) {
		--dn-halftone-art-ground: var(--dn-paper);
		right: -2.75rem;
		bottom: -3.25rem;
		width: 11rem;
		height: 11rem;
		color: color-mix(in oklab, var(--dn-paper-content) 80%, transparent);
	}
	.fe[data-look='dark'] :global(.fe__decor) {
		top: 0;
		right: -0.5rem;
		bottom: 0;
		width: 18%;
		max-width: 7.5rem;
		color: color-mix(in oklab, var(--dn-title) 30%, transparent);
	}
	.fe[data-look='band'] :global(.fe__decor) {
		left: 0;
		bottom: 0;
		width: 34%;
		max-width: 26rem;
		height: 75%;
		color: color-mix(in oklab, var(--dn-title) 34%, transparent);
	}

	.fe__stamp {
		display: grid;
		flex: none;
		place-items: center;
		width: 3.75rem;
		height: 3.75rem;
		border: 2px solid currentColor;
		border-radius: 0.625rem;
		color: var(--ink);
	}
	.fe[data-look='dark'] .fe__stamp,
	.fe[data-look='band'] .fe__stamp {
		background-color: var(--color-base-100);
	}

	.fe__title {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 2rem;
		line-height: 1.05;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		text-wrap: balance;
		font-synthesis: none;
		color: var(--dn-title);
	}
	.fe__desc {
		max-width: 27rem;
		margin-top: 0.5rem;
		margin-inline: auto;
		font-size: 0.9375rem;
		line-height: 1.5;
		text-wrap: pretty;
		color: var(--fe-muted, var(--color-base-content));
	}

	/* Your artists: stamp, rule, words and the button in one row */
	.fe[data-look='band'] {
		flex-direction: row;
		gap: 1.5rem;
		min-height: 8.5rem;
		padding: 1.5rem 2rem 1.5rem max(2rem, min(30%, 22rem));
		text-align: left;
	}
	.fe[data-look='band'] .fe__words {
		flex: 0 1 auto;
		min-width: 0;
	}
	.fe[data-look='band'] .fe__desc {
		margin-inline: 0;
		max-width: 34rem;
	}
	.fe__rule {
		flex: none;
		align-self: stretch;
		width: 2px;
		margin-block: 0.75rem;
		background-color: color-mix(in oklab, var(--dn-title) 60%, transparent);
	}
	.fe[data-look='band'] :global(.fe__cta) {
		flex: none;
		margin-left: 1rem;
	}

	/* the crowd is gone under high contrast, and so is its room */
	@media (prefers-contrast: more), (forced-colors: active) {
		.fe[data-look='band'] {
			padding-left: 2rem;
		}
	}

	@media (max-width: 1023px) {
		.fe[data-look='band'] {
			flex-wrap: wrap;
			padding-left: 2rem;
		}
		.fe[data-look='band'] :global(.fe__decor) {
			width: 55%;
			height: 60%;
		}
	}

	@media (max-width: 639px) {
		.fe {
			min-height: 13rem;
			padding: 1.75rem 1.25rem 2rem;
		}
		.fe__stamp {
			width: 3.25rem;
			height: 3.25rem;
		}
		.fe__title {
			font-size: 1.5rem;
		}
		.fe__desc {
			font-size: 0.875rem;
		}
		.fe[data-look='band'] {
			flex-direction: column;
			gap: 0.875rem;
			padding: 1.75rem 1.25rem 2rem;
			text-align: center;
		}
		.fe[data-look='band'] .fe__desc {
			margin-inline: auto;
		}
		.fe[data-look='band'] :global(.fe__cta) {
			margin-left: 0;
		}
		.fe__rule {
			display: none;
		}
		.fe[data-look='paper'] {
			padding-bottom: 3.5rem;
		}
		.fe[data-look='paper'] :global(.fe__decor) {
			width: 6.5rem;
			height: 6.5rem;
			right: -2.25rem;
			bottom: -2.5rem;
		}
		/* no room beside the words on a phone */
		.fe[data-look='dark'] :global(.fe__decor),
		.fe[data-look='band'] :global(.fe__decor) {
			display: none;
		}
	}
</style>
