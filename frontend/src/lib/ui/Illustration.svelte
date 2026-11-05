<!--
	The kit's small drawings, on the theme's tokens: a turntable with its
	tonearm, a dripping acid heart and a torn acid sticker with a folder.
	Decoration only: hidden from screen readers and pointers, gone under
	high contrast. `class` sizes it; the drawing keeps its proportions
	(`fit="cover"` fills the box and lets the edges go, for a drawing that
	runs off a tile).
-->
<script lang="ts">
	interface Props {
		name: 'turntable' | 'drip-heart' | 'folder-sticker';
		fit?: 'contain' | 'cover';
		/** Layout only: size and position. */
		class?: string;
	}

	let { name, fit = 'contain', class: className = '' }: Props = $props();

	const aspect = $derived(fit === 'cover' ? 'xMinYMid slice' : 'xMidYMid meet');
	const grooves = Array.from({ length: 13 }, (_, i) => 34 + i * 5.8);
	const spray = [
		[14, 22, 2.2],
		[8, 52, 1.6],
		[104, 14, 2],
		[112, 46, 2.6],
		[22, 92, 1.8],
		[98, 96, 1.4],
		[6, 34, 1.1],
		[116, 70, 1.2],
		[40, 6, 1.4],
		[86, 4, 1.8]
	] as const;
	const drips = [
		[38, 78, 6, 18],
		[56, 96, 7, 44],
		[70, 88, 5, 26],
		[84, 70, 6, 12]
	] as const;
</script>

{#if name === 'turntable'}
	<svg
		class="dn-decor dn-illo dn-illo--turntable {className}"
		viewBox="0 0 130 240"
		preserveAspectRatio={aspect}
		aria-hidden="true"
		focusable="false"
	>
		<circle cx="118" cy="122" r="108" class="vinyl" />
		{#each grooves as r (r)}
			<circle cx="118" cy="122" {r} class="groove" />
		{/each}
		<path d="M118 122 L16.5 85 A108 108 0 0 1 37.7 49.7 Z" class="gloss" />
		<path d="M118 122 L35.3 191.4 A108 108 0 0 1 17.9 162.5 Z" class="gloss" />
		<circle cx="118" cy="122" r="108" class="rim" />
		<circle cx="118" cy="122" r="26" class="label" />
		<circle cx="118" cy="122" r="3" class="vinyl" />
		<circle cx="124" cy="-2" r="16" class="arm-base" />
		<circle cx="124" cy="-2" r="7" class="arm-pin" />
		<path d="M124 -2 L80 104 L70 128" class="arm" />
		<path d="M124 -2 L80 104 L70 128" class="arm-line" />
		<g transform="rotate(24 66 140)">
			<rect x="55" y="124" width="22" height="32" rx="3" class="head" />
			<rect x="60" y="147" width="12" height="7" rx="1" class="cart" />
		</g>
	</svg>
{:else if name === 'drip-heart'}
	<svg
		class="dn-decor dn-illo dn-illo--heart {className}"
		viewBox="0 0 120 150"
		preserveAspectRatio={aspect}
		aria-hidden="true"
		focusable="false"
	>
		<path
			d="M60 38 C52 18 22 14 18 40 C15 60 38 76 60 96 C82 76 105 60 102 40 C98 14 68 18 60 38 Z"
			class="heart"
		/>
		{#each drips as [x, y, w, h] (x)}
			<rect {x} y={y - 4} width={w} height={h + 4} rx={w / 2} class="ink" />
			<circle cx={x + w / 2} cy={y + h} r={w * 0.62} class="ink" />
		{/each}
		{#each spray as [x, y, r] (`${x}-${y}`)}
			<circle cx={x} cy={y} {r} class="ink" />
		{/each}
	</svg>
{:else}
	<svg
		class="dn-decor dn-illo dn-illo--sticker {className}"
		viewBox="0 0 84 78"
		preserveAspectRatio={aspect}
		aria-hidden="true"
		focusable="false"
	>
		<path
			d="M3 6 L10 2 L18 5 L27 1 L36 4 L46 1 L55 4 L64 2 L72 5 L81 3 L79 12 L83 21 L80 31 L82 41 L79 50 L82 60 L79 69 L81 76 L71 73 L62 77 L52 74 L42 77 L32 74 L22 77 L13 74 L4 76 L6 66 L2 56 L5 46 L2 36 L5 26 L1 16 Z"
			class="sticker"
		/>
		<g transform="translate(18 15) scale(2)">
			<path
				d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"
				class="folder"
			/>
		</g>
	</svg>
{/if}

<style>
	.dn-illo {
		display: block;
		pointer-events: none;
		overflow: hidden;
	}

	.vinyl {
		fill: var(--dn-vinyl);
	}
	.groove {
		fill: none;
		stroke: rgb(255 255 255 / 0.11);
		stroke-width: 1.2;
	}
	.gloss {
		fill: rgb(255 255 255 / 0.07);
	}
	.rim {
		fill: none;
		stroke: rgb(255 255 255 / 0.22);
		stroke-width: 1.5;
	}
	.label {
		fill: var(--dn-vinyl-groove);
	}
	.arm-base {
		fill: color-mix(in oklab, var(--dn-paper) 80%, var(--dn-vinyl));
		stroke: var(--dn-vinyl);
		stroke-width: 2;
	}
	.arm-pin {
		fill: var(--dn-vinyl-groove);
	}
	.arm {
		fill: none;
		stroke: var(--dn-vinyl);
		stroke-width: 11;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
	.arm-line {
		fill: none;
		stroke: color-mix(in oklab, var(--dn-paper) 90%, var(--dn-vinyl));
		stroke-width: 6.5;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
	.head {
		fill: var(--dn-paper);
		stroke: var(--dn-vinyl);
		stroke-width: 2;
	}
	.cart {
		fill: var(--dn-vinyl);
	}

	.heart {
		fill: none;
		stroke: var(--dn-action);
		stroke-width: 15;
		stroke-linejoin: round;
	}
	.ink {
		fill: var(--dn-action);
	}
	:global([data-theme='light']) .dn-illo--heart {
		filter: drop-shadow(0 0 0.6px var(--dn-action-edge))
			drop-shadow(0 0 0.6px var(--dn-action-edge));
	}

	.sticker {
		fill: var(--dn-action);
	}
	.folder {
		fill: none;
		stroke: var(--dn-action-content);
		stroke-width: 2.2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
</style>
