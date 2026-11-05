<!--
	A small drawing printed as halftone dots in the current text colour, for
	the corner of an empty card: a `record`, a `crowd` of heads and raised
	hands, or a `map` fragment of street blocks. `fade` is the side it fades
	away toward (where the card's words sit). Decoration only: hidden from
	screen readers and pointers, gone under high contrast. `class` sizes,
	colours and places it; the drawing fills the box and lets its edges go.
-->
<script lang="ts">
	interface Props {
		name: 'record' | 'crowd' | 'map';
		fade?: 'left' | 'right' | 'top' | 'none';
		/** Layout only: size, position and colour. */
		class?: string;
	}

	let { name, fade = 'none', class: className = '' }: Props = $props();

	const uid = $props.id();
	const dots = `dn-halftone-art-dots-${uid}`;
	const fine = `dn-halftone-art-fine-${uid}`;

	const viewBox = { record: '0 0 200 200', crowd: '0 0 420 140', map: '0 0 220 220' };

	const grooves = Array.from({ length: 12 }, (_, i) => 40 + i * 4.6);

	// heads and shoulders across a stage front: [x, head y, head r]
	const people: [number, number, number][] = [
		[18, 74, 15],
		[52, 62, 17],
		[88, 80, 14],
		[118, 58, 18],
		[156, 76, 15],
		[188, 64, 16],
		[224, 82, 13],
		[252, 66, 17],
		[290, 78, 14],
		[322, 62, 16],
		[356, 80, 14],
		[392, 68, 16]
	];
	// raised hands: [shoulder x, shoulder y, hand x, hand y]
	const arms: [number, number, number, number][] = [
		[64, 92, 78, 22],
		[132, 90, 150, 14],
		[200, 94, 212, 30],
		[262, 96, 248, 18],
		[334, 92, 352, 24]
	];

	// street blocks on a slight tilt: [x, y, w, h]
	const blocks: [number, number, number, number][] = [
		[8, 6, 46, 30],
		[62, 6, 30, 30],
		[100, 6, 56, 30],
		[164, 6, 50, 30],
		[8, 44, 28, 52],
		[44, 44, 64, 24],
		[44, 76, 30, 20],
		[82, 76, 26, 20],
		[116, 44, 40, 52],
		[164, 44, 50, 22],
		[164, 74, 22, 22],
		[194, 74, 20, 22],
		[8, 104, 56, 34],
		[72, 104, 36, 34],
		[116, 104, 24, 62],
		[148, 104, 66, 30],
		[8, 146, 40, 26],
		[56, 146, 52, 26],
		[148, 142, 32, 30],
		[188, 142, 26, 30],
		[8, 180, 70, 34],
		[86, 180, 22, 34],
		[116, 174, 50, 40],
		[174, 180, 40, 34]
	];
</script>

<div class="dn-decor dn-halftone-art {className}" data-fade={fade} aria-hidden="true">
	<svg viewBox={viewBox[name]} preserveAspectRatio="xMidYMid slice">
		<defs>
			<pattern id={dots} width="4" height="4" patternUnits="userSpaceOnUse">
				<circle cx="2" cy="2" r="1.45" fill="currentColor" />
			</pattern>
			<pattern id={fine} width="4" height="4" patternUnits="userSpaceOnUse">
				<circle cx="2" cy="2" r="0.8" fill="currentColor" />
			</pattern>
		</defs>

		{#if name === 'record'}
			<circle cx="100" cy="100" r="97" fill="url(#{dots})" />
			{#each grooves as r (r)}
				<circle
					cx="100"
					cy="100"
					{r}
					fill="none"
					stroke="currentColor"
					stroke-width="0.9"
					opacity="0.55"
				/>
			{/each}
			<!-- light caught across the grooves -->
			<path
				d="M100 100 L168 18 A97 97 0 0 1 192 60 Z"
				fill="url(#{fine})"
				class="dn-halftone-art__knock"
			/>
			<circle cx="100" cy="100" r="31" class="dn-halftone-art__knock" />
			<circle cx="100" cy="100" r="31" fill="url(#{fine})" />
			<circle cx="100" cy="100" r="31" fill="none" stroke="currentColor" stroke-width="1.4" />
			<circle cx="100" cy="100" r="3.5" fill="currentColor" />
		{:else if name === 'crowd'}
			{#each arms as [sx, sy, hx, hy] (sx)}
				<path
					d="M{sx} {sy} L{hx} {hy}"
					stroke="url(#{dots})"
					stroke-width="9"
					stroke-linecap="round"
				/>
				<circle cx={hx} cy={hy - 3} r="7" fill="url(#{dots})" />
			{/each}
			{#each people as [x, y, r] (x)}
				<circle cx={x} cy={y} {r} fill="url(#{dots})" />
				<path
					d="M{x - r * 2.2} 140 C{x - r * 2.1} {y + r * 1.6} {x - r * 0.9} {y + r * 1.05} {x} {y +
						r * 1.05} C{x + r * 0.9} {y + r * 1.05} {x + r * 2.1} {y + r * 1.6} {x + r * 2.2} 140 Z"
					fill="url(#{dots})"
				/>
			{/each}
		{:else}
			<g transform="rotate(-14 110 110)">
				{#each blocks as [x, y, w, h], i (i)}
					<rect {x} {y} width={w} height={h} rx="1.5" fill="url(#{i % 3 === 1 ? fine : dots})" />
				{/each}
				<path
					d="M-20 128 C40 120 70 150 120 140 S190 118 240 128"
					fill="none"
					stroke="currentColor"
					stroke-width="5"
					opacity="0.8"
				/>
			</g>
		{/if}
	</svg>
</div>

<style>
	.dn-halftone-art {
		position: relative;
		overflow: hidden;
		pointer-events: none;
	}
	.dn-halftone-art svg {
		display: block;
		width: 100%;
		height: 100%;
	}
	/* the card behind shows through: light on the record, the label's plate */
	.dn-halftone-art__knock {
		fill: var(--dn-halftone-art-ground, transparent);
	}

	.dn-halftone-art[data-fade='left'] {
		mask-image: linear-gradient(to left, #000 35%, transparent);
		-webkit-mask-image: linear-gradient(to left, #000 35%, transparent);
	}
	.dn-halftone-art[data-fade='right'] {
		mask-image: linear-gradient(to right, #000 35%, transparent);
		-webkit-mask-image: linear-gradient(to right, #000 35%, transparent);
	}
	.dn-halftone-art[data-fade='top'] {
		mask-image: linear-gradient(to top, #000 40%, transparent);
		-webkit-mask-image: linear-gradient(to top, #000 40%, transparent);
	}
</style>
