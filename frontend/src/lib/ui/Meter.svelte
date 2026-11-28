<!--
	A needle meter on a cream face, swinging between two words (`low` at the
	left, `high` at the right) with no numbers. `value` runs from 0 (`low`)
	to 1 (`high`); the needle swings to it. It is one image named by
	`label`, so whatever reads it as a sentence says in words what it shows.
	The face stays cream in every theme, and the arc's last stretch towards
	`high` is in sound's plum.
-->
<script lang="ts">
	interface Props {
		/** The sentence the picture is named by: "Sounds very close". */
		label: string;
		/** 0 at `low`, 1 at `high`. */
		value: number;
		low?: string;
		high?: string;
		/** Printed under the face. */
		caption?: string;
		/** Layout only: margin, width. */
		class?: string;
	}

	let {
		label,
		value,
		low = 'Far',
		high = 'Close',
		caption,
		class: className = ''
	}: Props = $props();

	/** The needle's sweep, either side of upright, in degrees. */
	const SWING = 55;
	const angle = $derived(-SWING + Math.min(1, Math.max(0, value)) * SWING * 2);
</script>

<div class="dn-meter {className}">
	<div class="dn-meter__face" role="img" aria-label={label}>
		<svg viewBox="0 0 120 64" aria-hidden="true">
			<path class="dn-meter__arc" d="M18 52 A46 46 0 0 1 102 52" />
			<path class="dn-meter__hot" d="M88 22 A46 46 0 0 1 102 52" />
		</svg>
		<span class="dn-meter__needle" style:rotate="{angle}deg" aria-hidden="true"></span>
		<span class="dn-meter__hub" aria-hidden="true"></span>
		<span class="dn-meter__word" data-at="low" aria-hidden="true">{low}</span>
		<span class="dn-meter__word" data-at="high" aria-hidden="true">{high}</span>
	</div>
	{#if caption}
		<span class="dn-meter__caption" aria-hidden="true">{caption}</span>
	{/if}
</div>

<style>
	.dn-meter {
		display: inline-flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
	}
	.dn-meter__face {
		position: relative;
		overflow: hidden;
		width: 9rem;
		aspect-ratio: 120 / 70;
		border-radius: 0.375rem;
		background: linear-gradient(
			175deg,
			var(--dn-arm),
			color-mix(in oklab, var(--dn-arm) 88%, black)
		);
		box-shadow:
			inset 0 1px 3px rgb(0 0 0 / 0.35),
			0 0 0 3px color-mix(in oklab, var(--dn-vinyl) 85%, transparent),
			0 2px 6px rgb(0 0 0 / 0.5);
		color: var(--dn-vinyl);
	}
	.dn-meter__face svg {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}
	.dn-meter__arc,
	.dn-meter__hot {
		fill: none;
		stroke-linecap: round;
	}
	.dn-meter__arc {
		stroke: var(--dn-vinyl);
		stroke-width: 1.2;
	}
	.dn-meter__hot {
		stroke: color-mix(in oklab, var(--dn-sound) 65%, black);
		stroke-width: 3.5;
	}
	.dn-meter__needle {
		position: absolute;
		left: 50%;
		bottom: 12%;
		width: 2px;
		height: 66%;
		margin-left: -1px;
		border-radius: 1px;
		background: var(--dn-vinyl);
		transform-origin: 50% 100%;
		transition: rotate 0.62s cubic-bezier(0.3, 1.4, 0.5, 1);
	}
	.dn-meter__hub {
		position: absolute;
		left: 50%;
		bottom: 0;
		width: 1.375rem;
		height: 1.375rem;
		translate: -50% 45%;
		border-radius: 9999px;
		background: var(--dn-vinyl);
	}
	.dn-meter__word {
		position: absolute;
		bottom: 0.3rem;
		font-family: var(--font-mono);
		font-size: 0.5625rem;
		font-weight: 700;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.dn-meter__word[data-at='low'] {
		left: 0.5rem;
	}
	.dn-meter__word[data-at='high'] {
		right: 0.5rem;
	}
	.dn-meter__caption {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}

	@media (forced-colors: active) {
		.dn-meter__face {
			border: 1px solid CanvasText;
		}
		.dn-meter__needle {
			background: CanvasText;
		}
	}
</style>
