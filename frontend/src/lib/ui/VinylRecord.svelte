<!--
	A large glossy record, drawn: fine grooves, a bright rim and a sheen
	where light catches it. `sheen="acid"` is a page header's record, a
	broad acid wedge with a white gloss beside it; `soft` is plain white
	gloss. `label` puts
	the acid centre label on. Decoration only: hidden from screen readers and
	pointers, still, gone under high contrast. `class` sizes and places it
	(a square; the caller clips it).
-->
<script lang="ts">
	interface Props {
		sheen?: 'acid' | 'soft';
		label?: boolean;
		/** Layout only: size and position. */
		class?: string;
	}

	let { sheen = 'soft', label = false, class: className = '' }: Props = $props();

	const uid = $props.id();
	const arcId = `dn-record-arc-${uid}`;
</script>

<div class="dn-decor dn-record {className}" aria-hidden="true">
	<div class="dn-record__disc" data-sheen={sheen}>
		{#if label}
			<div class="dn-record__label">
				<svg viewBox="0 0 100 100">
					<defs>
						<path id={arcId} d="M50 50 m-34 0 a34 34 0 1 1 68 0 a34 34 0 1 1 -68 0" />
					</defs>
					<text>
						<textPath href="#{arcId}" startOffset="4%">DROPPEDNEEDLE · SIDE A · 33⅓</textPath>
					</text>
				</svg>
			</div>
		{/if}
	</div>
</div>

<style>
	.dn-record {
		container-type: inline-size;
		aspect-ratio: 1;
		pointer-events: none;
	}

	.dn-record__disc {
		--sheen: rgb(255 255 255 / 0.16);
		position: relative;
		width: 100%;
		height: 100%;
		border-radius: 9999px;
		background:
			radial-gradient(circle, var(--dn-vinyl) 0 1.2%, transparent 1.5%),
			radial-gradient(ellipse 30% 14% at 72% 18%, rgb(255 255 255 / 0.08), transparent 100%),
			conic-gradient(
				transparent 0deg,
				var(--sheen) 34deg,
				transparent 62deg,
				transparent 150deg,
				rgb(255 255 255 / 0.06) 172deg,
				transparent 196deg,
				transparent 214deg,
				var(--sheen) 236deg,
				transparent 262deg
			),
			repeating-radial-gradient(
				circle,
				transparent 0,
				rgb(255 255 255 / 0.05) 0.18cqw,
				transparent 0.42cqw
			),
			radial-gradient(
				circle,
				var(--dn-vinyl) 0 32%,
				color-mix(in oklab, var(--dn-vinyl-groove) 40%, var(--dn-vinyl)) 66%,
				var(--dn-vinyl) 94%,
				var(--dn-vinyl-groove) 98.6%,
				var(--dn-vinyl) 100%
			);
		box-shadow:
			inset 0 0 0 1px rgb(255 255 255 / 0.14),
			0 18px 40px -16px rgb(0 0 0 / 0.7);
	}

	/* acid light sweeping round the right side and the bottom, brightest at
	   the rim and cut by dark grooves; the centre fades into the page */
	/* The header's record: deep black under fine grooves, a broad acid
	   wedge of light across them with a white gloss beside it, both
	   brightest toward the rim. `--sheen-from` on the record turns the
	   light round to the part the page shows. Black in both themes. */
	.dn-record__disc[data-sheen='acid'] {
		--acid: var(--dn-action);
		background:
			repeating-radial-gradient(
				circle closest-side,
				rgb(0 0 0 / 0.55) 0 0.25cqw,
				transparent 0.25cqw 0.7cqw
			),
			repeating-radial-gradient(
				circle closest-side,
				transparent 0 1.3cqw,
				rgb(0 0 0 / 0.45) 1.3cqw 2.1cqw,
				transparent 2.1cqw 3.4cqw,
				rgb(0 0 0 / 0.25) 3.4cqw 3.9cqw,
				transparent 3.9cqw 5.3cqw
			),
			radial-gradient(
				circle closest-side,
				var(--dn-vinyl) 0 18%,
				color-mix(in oklab, var(--dn-vinyl) 40%, transparent) 42%,
				transparent 70%
			),
			conic-gradient(
				from var(--sheen-from, 40deg),
				transparent 0deg,
				color-mix(in oklab, var(--acid) 45%, transparent) 14deg,
				var(--acid) 28deg,
				var(--acid) 56deg,
				color-mix(in oklab, var(--acid) 45%, transparent) 74deg,
				transparent 92deg,
				transparent 98deg,
				rgb(255 255 255 / 0.4) 114deg,
				rgb(255 255 255 / 0.55) 124deg,
				transparent 148deg,
				transparent 360deg
			),
			radial-gradient(
				circle closest-side,
				var(--dn-vinyl) 0 62%,
				color-mix(in oklab, var(--dn-vinyl-groove) 55%, var(--dn-vinyl)) 90%,
				var(--dn-vinyl) 99%
			);
		box-shadow:
			inset 0 0 0 1px rgb(255 255 255 / 0.12),
			0 18px 40px -16px rgb(0 0 0 / 0.6);
	}

	.dn-record__label {
		position: absolute;
		inset: 29%;
		border-radius: 9999px;
		background:
			radial-gradient(circle, var(--dn-vinyl) 0 4%, transparent 4.6%),
			radial-gradient(circle at 35% 30%, rgb(255 255 255 / 0.18), transparent 60%),
			color-mix(in oklab, var(--dn-action) 72%, var(--dn-vinyl-groove));
		box-shadow: inset 0 0 0 0.4cqw color-mix(in oklab, var(--dn-action), #000 45%);
	}
	.dn-record__label svg {
		width: 100%;
		height: 100%;
		rotate: -20deg;
	}
	.dn-record__label text {
		fill: color-mix(in oklab, var(--dn-action-content) 70%, transparent);
		font-family: var(--font-mono);
		font-size: 6.4px;
		font-weight: 600;
		letter-spacing: 0.08em;
	}
</style>
