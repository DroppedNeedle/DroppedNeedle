<!--
	A photo printed as a black-and-white halftone that fades into the page:
	grey and hard, under a fine dot screen, its edges masked away toward
	`fade` (the sides the page's words sit on). CSS only, so a picture from
	another host works. In light it prints dark on the paper. Until the photo
	has loaded (or when it never does) the box shows `fallback` big in Anton
	on a dot field, never a grey box. Under high contrast and forced colours
	the plain photo shows, with no screen and no fade. `children` is the
	picture, filling the box; `class` places and sizes it.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		/** Whether the picture has loaded; until then the fallback shows. */
		loaded: boolean;
		/** A word or initials printed when there is no picture. */
		fallback: string;
		/** The sides that fade into the page. */
		fade?: 'left-bottom' | 'bottom';
		/** Layout only: position and size. */
		class?: string;
		children: Snippet;
	}

	let { loaded, fallback, fade = 'left-bottom', class: className = '', children }: Props = $props();
</script>

<div class="dn-halftone {className}" data-fade={fade} data-loaded={loaded}>
	<div class="dn-halftone__print">
		{#if !loaded}
			<div class="dn-halftone__fallback" aria-hidden="true">
				<span class="dn-halftone__field"></span>
				<span class="dn-halftone__initials">{fallback}</span>
			</div>
		{/if}
		<div class="dn-halftone__photo">
			{@render children()}
		</div>
		<span class="dn-decor dn-halftone__screen" aria-hidden="true"></span>
	</div>
</div>

<style>
	.dn-halftone {
		--ink: var(--color-base-100);
		position: relative;
		overflow: hidden;
		pointer-events: none;
		/* in dark the light of the photo adds to the page, so anything behind
		   it (the page's dots) shows through its shadows; in light the ink
		   darkens the paper */
		mix-blend-mode: lighten;
	}
	:global([data-theme='light']) .dn-halftone {
		mix-blend-mode: multiply;
	}
	.dn-halftone__print {
		position: absolute;
		inset: 0;
		mask-image:
			linear-gradient(to right, transparent 0, #000 30%),
			linear-gradient(to top, transparent 0, #000 35%);
		mask-composite: intersect;
		-webkit-mask-image:
			linear-gradient(to right, transparent 0, #000 30%),
			linear-gradient(to top, transparent 0, #000 35%);
		-webkit-mask-composite: source-in;
	}
	.dn-halftone[data-fade='bottom'] .dn-halftone__print {
		mask-image: linear-gradient(to top, transparent 0, #000 55%);
		-webkit-mask-image: linear-gradient(to top, transparent 0, #000 55%);
	}

	.dn-halftone__photo {
		position: absolute;
		inset: 0;
		opacity: 0;
		transition: opacity 400ms;
	}
	.dn-halftone[data-loaded='true'] .dn-halftone__photo {
		opacity: 1;
	}
	.dn-halftone__photo :global(img) {
		width: 100%;
		height: 100%;
		object-fit: cover;
		object-position: 50% 22%;
		filter: grayscale(1) brightness(1.55) contrast(1.6);
	}
	:global([data-theme='light']) .dn-halftone__photo :global(img) {
		filter: grayscale(1) contrast(1.7) brightness(0.98);
	}

	/* the dot screen: the page's own colour between round windows, so the
	   picture reads as dots of ink */
	.dn-halftone__screen {
		position: absolute;
		inset: 0;
		background-image: radial-gradient(
			circle at center,
			transparent 0 1.15px,
			color-mix(in oklab, var(--ink) 42%, transparent) 1.9px
		);
		background-size: 3.5px 3.5px;
	}
	:global([data-theme='light']) .dn-halftone__screen {
		background-image: radial-gradient(
			circle at center,
			transparent 0 1.05px,
			color-mix(in oklab, var(--ink) 24%, transparent) 1.8px
		);
	}

	.dn-halftone__fallback {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		container-type: size;
	}
	.dn-halftone__field {
		position: absolute;
		inset: 0;
		background-image: radial-gradient(circle at center, var(--dn-frit) 0 1.6px, transparent 2px);
		background-size: 9px 9px;
		opacity: var(--dn-frit-opacity);
		mask-image: radial-gradient(ellipse 70% 70% at 62% 40%, #000, transparent 75%);
		-webkit-mask-image: radial-gradient(ellipse 70% 70% at 62% 40%, #000, transparent 75%);
	}
	.dn-halftone__initials {
		position: relative;
		font-family: var(--font-display);
		font-size: min(70cqh, 48cqw);
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		font-synthesis: none;
		color: color-mix(in oklab, var(--color-base-content) 16%, transparent);
		translate: 12% -4%;
	}

	@media (prefers-contrast: more), (forced-colors: active) {
		.dn-halftone__print {
			mask-image: none;
			-webkit-mask-image: none;
		}
		.dn-halftone,
		:global([data-theme]) .dn-halftone {
			mix-blend-mode: normal;
		}
		.dn-halftone__photo :global(img),
		:global([data-theme]) .dn-halftone__photo :global(img) {
			filter: none;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.dn-halftone__photo {
			transition: none;
		}
	}
</style>
