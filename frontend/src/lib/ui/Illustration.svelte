<!--
	The kit's small drawings, on the theme's tokens: a turntable with its
	tonearm, a dripping acid heart, a torn acid sticker with a folder, and
	three printed in halftone ink for a paper card: a crate of records, two
	taped boxes marked TAGS and ARTWORK, and a fingerprint.
	Decoration only: hidden from screen readers and pointers, gone under
	high contrast. `class` sizes it; the drawing keeps its proportions
	(`fit="cover"` fills the box and lets the edges go, for a drawing that
	runs off a tile).
-->
<script lang="ts">
	interface Props {
		name:
			'turntable' | 'drip-heart' | 'folder-sticker' | 'record-crate' | 'tag-boxes' | 'fingerprint';
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
	// the halftone prints share one dot screen, named per drawing
	const dotsId = $props.id();
	const sleeves = [
		[10, 26, -14],
		[28, 20, -8],
		[46, 16, -3],
		[64, 16, 3],
		[80, 22, 9]
	] as const;
	const ridges = Array.from({ length: 9 }, (_, i) => i);
</script>

{#snippet dots()}
	<defs>
		<pattern id={dotsId} width="3.2" height="3.2" patternUnits="userSpaceOnUse">
			<circle cx="1.6" cy="1.6" r="0.95" class="ink" />
		</pattern>
	</defs>
{/snippet}

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
{:else if name === 'record-crate'}
	<svg
		class="dn-decor dn-illo dn-illo--print {className}"
		viewBox="0 0 130 120"
		preserveAspectRatio={aspect}
		aria-hidden="true"
		focusable="false"
	>
		{@render dots()}
		{#each sleeves as [x, y, turn] (x)}
			<g transform="rotate({turn} {x + 22} 100)">
				<rect {x} {y} width="44" height="70" class="sleeve" />
				<rect {x} {y} width="44" height="70" class="screen" fill="url(#{dotsId})" />
				<rect {x} {y} width="44" height="70" class="line" />
				<circle cx={x + 22} cy={y + 24} r="12" class="line" />
				<circle cx={x + 22} cy={y + 24} r="3" class="ink" />
			</g>
		{/each}
		<path d="M4 68 L126 68 L120 116 L10 116 Z" class="crate" />
		<path d="M4 68 L126 68 L120 116 L10 116 Z" class="screen" fill="url(#{dotsId})" />
		<path d="M4 68 L126 68 L120 116 L10 116 Z" class="line heavy" />
		<path d="M7 84 L123 84 M9 100 L121 100" class="line" />
		<rect x="48" y="73" width="34" height="7" rx="3.5" class="ink" />
	</svg>
{:else if name === 'tag-boxes'}
	<svg
		class="dn-decor dn-illo dn-illo--print {className}"
		viewBox="0 0 130 130"
		preserveAspectRatio={aspect}
		aria-hidden="true"
		focusable="false"
	>
		{@render dots()}
		<g transform="rotate(-4 64 40)">
			<path d="M20 12 L104 6 L108 64 L24 70 Z" class="crate" />
			<path d="M20 12 L104 6 L108 64 L24 70 Z" class="screen" fill="url(#{dotsId})" />
			<path d="M20 12 L104 6 L108 64 L24 70 Z" class="line heavy" />
			<path d="M58 9 L66 8.5 L70 66 L62 66.6 Z" class="tape" />
			<g transform="rotate(-8 64 38)">
				<rect x="38" y="28" width="50" height="19" class="label" />
				<rect x="38" y="28" width="50" height="19" class="line" />
				<text x="63" y="42.5" class="stencil">TAGS</text>
			</g>
		</g>
		<g transform="rotate(5 66 98)">
			<path d="M10 72 L118 74 L116 126 L12 124 Z" class="crate" />
			<path d="M10 72 L118 74 L116 126 L12 124 Z" class="screen" fill="url(#{dotsId})" />
			<path d="M10 72 L118 74 L116 126 L12 124 Z" class="line heavy" />
			<path d="M10 72 L118 74 L117.8 81 L10.2 79 Z" class="tape" />
			<g transform="rotate(-6 64 100)">
				<rect x="26" y="91" width="76" height="19" class="label" />
				<rect x="26" y="91" width="76" height="19" class="line" />
				<text x="64" y="105.5" class="stencil">ARTWORK</text>
			</g>
		</g>
	</svg>
{:else if name === 'fingerprint'}
	<svg
		class="dn-decor dn-illo dn-illo--print {className}"
		viewBox="0 0 110 130"
		preserveAspectRatio={aspect}
		aria-hidden="true"
		focusable="false"
	>
		{@render dots()}
		<ellipse cx="55" cy="66" rx="52" ry="62" class="screen" fill="url(#{dotsId})" />
		{#each ridges as i (i)}
			<ellipse
				cx="55"
				cy={70 - i * 0.6}
				rx={5 + i * 5.3}
				ry={8 + i * 6.5}
				class="ridge"
				stroke-dasharray="{18 + i * 9} {3 + (i % 3) * 2}"
				stroke-dashoffset={i * 7}
			/>
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

	/* the halftone prints: dark ink on the paper they sit on */
	.dn-illo--print {
		color: var(--dn-paper-content);
	}
	.dn-illo--print .ink {
		fill: currentColor;
	}
	.dn-illo--print .sleeve,
	.dn-illo--print .label {
		fill: var(--dn-paper);
	}
	.dn-illo--print .crate {
		fill: color-mix(in oklab, currentColor 10%, var(--dn-paper));
	}
	.dn-illo--print .screen {
		opacity: 0.5;
	}
	.dn-illo--print .tape {
		fill: color-mix(in oklab, currentColor 28%, var(--dn-paper));
	}
	.dn-illo--print .line,
	.dn-illo--print .ridge {
		fill: none;
		stroke: currentColor;
		stroke-width: 1.6;
		stroke-linejoin: round;
	}
	.dn-illo--print .heavy {
		stroke-width: 2.4;
	}
	.dn-illo--print .ridge {
		stroke-width: 3.2;
		stroke-linecap: round;
	}
	.dn-illo--print .stencil {
		fill: currentColor;
		font-family: var(--font-display);
		font-size: 12px;
		letter-spacing: 0.06em;
		text-anchor: middle;
	}
	.folder {
		fill: none;
		stroke: var(--dn-action-content);
		stroke-width: 2.2;
		stroke-linecap: round;
		stroke-linejoin: round;
	}
</style>
