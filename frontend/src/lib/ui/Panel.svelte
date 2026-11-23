<!--
	A piece of hardware with an edge: a bevelled panel that holds everything
	set on it. `metal` is the Listening Room's brushed plate with a screw in
	each corner; `mat` the matte counter, a faint grid printed near its edges
	and one inner rule; `tray` the inset tray set into either. The screws,
	the grid and the mat's sheen are decoration and go under high contrast.
	`highlight` lights a metal or mat edge in the action colour, for a panel
	something is being dragged onto. Descendants can read the panel's metal
	as `--panel-metal` and its top bevel as `--panel-bevel-hi`.
	A caller's attributes and handlers pass through to the panel's own
	element (`as` picks it), so a `section` panel can carry its name.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { HTMLAttributes } from 'svelte/elements';

	interface Props extends Omit<HTMLAttributes<HTMLElement>, 'children'> {
		look?: 'metal' | 'mat' | 'tray';
		/** `lg` is the deck's (roomier from 640px), `md` the counter's, `sm` a tray's. */
		padding?: 'none' | 'sm' | 'md' | 'lg';
		highlight?: boolean;
		as?: 'div' | 'section';
		ref?: HTMLElement | null;
		children?: Snippet;
	}

	let {
		look = 'metal',
		padding = 'lg',
		highlight = false,
		as = 'div',
		ref = $bindable(null),
		class: className = '',
		children,
		...rest
	}: Props = $props();
</script>

<svelte:element
	this={as}
	bind:this={ref}
	{...rest}
	class="dn-panel {className}"
	data-look={look}
	data-padding={padding}
	class:is-lit={highlight}
>
	{#if look === 'metal'}
		<span class="dn-panel__screw dn-decor" data-at="tl" aria-hidden="true"></span>
		<span class="dn-panel__screw dn-decor" data-at="tr" aria-hidden="true"></span>
		<span class="dn-panel__screw dn-decor" data-at="bl" aria-hidden="true"></span>
		<span class="dn-panel__screw dn-decor" data-at="br" aria-hidden="true"></span>
	{:else if look === 'mat'}
		<span class="dn-panel__sheen dn-decor" aria-hidden="true"></span>
		<span class="dn-panel__grid dn-decor" aria-hidden="true"></span>
	{/if}
	{@render children?.()}
</svelte:element>

<style>
	.dn-panel {
		--panel-metal: var(--color-base-200);
		--panel-metal-hi: var(--color-base-300);
		--panel-bevel-hi: rgb(255 255 255 / 0.07);
		--panel-bevel-lo: rgb(0 0 0 / 0.55);
		--panel-brush: rgb(255 255 255 / 0.018);
	}
	/* a tray is not positioned, so it paints in the flow of what holds it */
	.dn-panel:is([data-look='metal'], [data-look='mat']) {
		position: relative;
		outline: 2px solid transparent;
		outline-offset: 4px;
		transition: outline-color 0.2s ease;
	}
	:global([data-theme='light']) .dn-panel {
		--panel-metal: var(--color-base-300);
		--panel-metal-hi: var(--color-base-200);
		--panel-bevel-hi: rgb(255 255 255 / 0.9);
		--panel-bevel-lo: rgb(0 0 0 / 0.08);
		--panel-brush: rgb(0 0 0 / 0.025);
	}
	.dn-panel.is-lit {
		outline-color: var(--dn-action);
	}

	.dn-panel[data-padding='sm'] {
		padding: 1rem;
	}
	.dn-panel[data-padding='md'] {
		padding: 1.375rem;
	}
	.dn-panel[data-padding='lg'] {
		padding: 1.25rem;
	}
	@media (min-width: 640px) {
		.dn-panel[data-padding='lg'] {
			padding: 1.75rem;
		}
	}

	/* brushed metal in the theme's own colours */
	.dn-panel[data-look='metal'] {
		border-radius: 1.25rem;
		border: 1px solid var(--dn-line);
		background:
			repeating-linear-gradient(
				0deg,
				var(--panel-brush) 0 1px,
				transparent 1px 3px,
				rgb(0 0 0 / 0.02) 3px 4px
			),
			linear-gradient(
				172deg,
				color-mix(in oklab, var(--panel-metal-hi) 100%, rgb(255 255 255 / 0.04)) 0%,
				var(--panel-metal) 46%,
				color-mix(in oklab, var(--panel-metal) 80%, var(--color-base-100)) 100%
			);
		box-shadow:
			inset 0 1px 0 var(--panel-bevel-hi),
			inset 0 -2px 0 var(--panel-bevel-lo),
			inset 0 0 0 4px color-mix(in oklab, var(--panel-metal) 60%, transparent),
			0 28px 50px -32px rgb(0 0 0 / 0.9);
	}
	:global([data-theme='light']) .dn-panel[data-look='metal'] {
		box-shadow:
			inset 0 1px 0 var(--panel-bevel-hi),
			inset 0 -2px 0 var(--panel-bevel-lo),
			0 24px 40px -30px rgb(0 0 0 / 0.35);
	}

	.dn-panel__screw {
		position: absolute;
		width: 0.75rem;
		height: 0.75rem;
		border-radius: 9999px;
		background:
			linear-gradient(
				var(--slot, 45deg),
				transparent 42%,
				rgb(0 0 0 / 0.55) 42% 58%,
				transparent 58%
			),
			radial-gradient(circle at 35% 30%, rgb(255 255 255 / 0.35), transparent 70%),
			color-mix(in oklab, var(--color-base-content) 30%, var(--panel-metal));
		box-shadow:
			inset 0 0 0 1px rgb(0 0 0 / 0.35),
			0 1px 0 var(--panel-bevel-hi);
		pointer-events: none;
	}
	.dn-panel__screw[data-at='tl'] {
		top: 0.625rem;
		left: 0.625rem;
		--slot: 30deg;
	}
	.dn-panel__screw[data-at='tr'] {
		top: 0.625rem;
		right: 0.625rem;
		--slot: 115deg;
	}
	.dn-panel__screw[data-at='bl'] {
		bottom: 0.625rem;
		left: 0.625rem;
		--slot: 160deg;
	}
	.dn-panel__screw[data-at='br'] {
		bottom: 0.625rem;
		right: 0.625rem;
		--slot: 70deg;
	}

	/* the matte counter: bevelled, unbrushed, with a soft light from the top left */
	.dn-panel[data-look='mat'] {
		--panel-grid: color-mix(in oklab, var(--dn-title) 9%, transparent);
		/* the sheen and grid sit under what lies on the mat, above its face */
		isolation: isolate;
		border-radius: 1.375rem;
		border: 1px solid var(--dn-line);
		background: linear-gradient(175deg, var(--dn-mat-hi), var(--dn-mat) 55%);
		box-shadow:
			inset 0 1px 0 var(--panel-bevel-hi),
			inset 0 -2px 0 var(--panel-bevel-lo),
			0 30px 50px -34px rgb(0 0 0 / 0.9);
	}
	:global([data-theme='light']) .dn-panel[data-look='mat'] {
		--panel-grid: color-mix(in oklab, var(--dn-title) 12%, transparent);
		box-shadow:
			inset 0 1px 0 var(--panel-bevel-hi),
			inset 0 -2px 0 var(--panel-bevel-lo),
			0 24px 40px -30px rgb(0 0 0 / 0.35);
	}
	.dn-panel[data-look='mat'] > .dn-panel__sheen {
		position: absolute;
		z-index: -1;
		inset: 0;
		border-radius: inherit;
		background: radial-gradient(90% 70% at 22% 0%, rgb(255 255 255 / 0.045), transparent 62%);
		pointer-events: none;
	}
	/* the printed grid shows only towards the edges, inside one rule */
	.dn-panel[data-look='mat'] > .dn-panel__grid {
		position: absolute;
		z-index: -1;
		inset: 0.5625rem;
		border-radius: 0.9375rem;
		border: 1px solid var(--panel-grid);
		background-image:
			linear-gradient(to right, var(--panel-grid) 1px, transparent 1px),
			linear-gradient(to bottom, var(--panel-grid) 1px, transparent 1px);
		background-size: 1.625rem 1.625rem;
		mask-image: radial-gradient(ellipse 62% 70% at 50% 50%, transparent 78%, black 100%);
		-webkit-mask-image: radial-gradient(ellipse 62% 70% at 50% 50%, transparent 78%, black 100%);
		pointer-events: none;
	}

	/* an inset tray set into the panel */
	.dn-panel[data-look='tray'] {
		border-radius: 0.875rem;
		background: color-mix(in oklab, var(--color-base-100) 70%, transparent);
		box-shadow:
			inset 0 2px 6px rgb(0 0 0 / 0.45),
			inset 0 0 0 1px rgb(0 0 0 / 0.35),
			0 1px 0 var(--panel-bevel-hi);
	}
	:global([data-theme='light']) .dn-panel[data-look='tray'] {
		background: color-mix(in oklab, var(--color-base-100) 80%, var(--color-base-300));
		box-shadow:
			inset 0 2px 5px rgb(0 0 0 / 0.1),
			inset 0 0 0 1px rgb(0 0 0 / 0.08),
			0 1px 0 var(--panel-bevel-hi);
	}

	@media (forced-colors: active) {
		.dn-panel:is([data-look='metal'], [data-look='mat']):not(.is-lit) {
			outline: none;
		}
	}
	@media (prefers-contrast: more) {
		.dn-panel:is([data-look='metal'], [data-look='mat']) {
			background: var(--color-base-200);
			border-color: var(--color-base-content);
		}
	}
</style>
