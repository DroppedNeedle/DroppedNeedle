<!--
	A rotary control: a black machined dial with an acid tick over a dotted
	scale that lights up to the value (from `origin`, so a tone knob lights
	from its centre). To assistive tech it is a slider: arrow keys step,
	Page Up and Page Down move a tenth of the range, Home and End go to the
	ends. It turns by dragging up and down, or round its edge, and a double
	click returns it to `resetTo` when one is given. Over the face, the
	mouse wheel turns it one arrow-key step per notch. `label` is its
	accessible name and is printed under it with the value, which `format`
	writes for the readout and for screen readers.
-->
<script lang="ts">
	interface Props {
		label: string;
		value: number;
		onchange: (value: number) => void;
		min?: number;
		max?: number;
		/** What a drag rounds to. */
		step?: number;
		/** What an arrow key moves; `step` when not given. */
		keyStep?: number;
		/** Where the lit part of the scale starts; `min` when not given. */
		origin?: number;
		/** The value a double click returns to. */
		resetTo?: number;
		/** Dots on the scale, end to end. */
		dots?: number;
		/** Numbers printed beside the scale, at a value each. */
		marks?: { at: number; text: string }[];
		size?: 'sm' | 'lg';
		format?: (value: number) => string;
		disabled?: boolean;
		/** Says why it is disabled, as a tooltip. */
		disabledReason?: string;
		/** Layout only: margin, flex. */
		class?: string;
	}

	let {
		label,
		value,
		onchange,
		min = 0,
		max = 100,
		step = 1,
		keyStep,
		origin,
		resetTo,
		dots = 11,
		marks = [],
		size = 'lg',
		format = (v: number) => String(v),
		disabled = false,
		disabledReason,
		class: className = ''
	}: Props = $props();

	const SWEEP = 270;
	const range = $derived(Math.max(max - min, Number.EPSILON));
	const clamped = $derived(Math.min(max, Math.max(min, value)));
	const angleOf = (v: number) => -SWEEP / 2 + ((v - min) / range) * SWEEP;
	const angle = $derived(angleOf(clamped));

	// the scale in a 120 unit box: dots on a ring, numbers just outside it
	const C = 60;
	const DOT_R = 49;
	const MARK_R = 58;
	function point(deg: number, r: number) {
		const rad = (deg * Math.PI) / 180;
		return { x: C + r * Math.sin(rad), y: C - r * Math.cos(rad) };
	}
	const scale = $derived.by(() => {
		const from = Math.min(origin ?? min, clamped);
		const to = Math.max(origin ?? min, clamped);
		return Array.from({ length: dots }, (_, i) => {
			const at = min + (range * i) / Math.max(dots - 1, 1);
			const lit = at >= from - range * 1e-6 && at <= to + range * 1e-6;
			return { ...point(angleOf(at), DOT_R), lit, key: i };
		});
	});
	const printed = $derived(
		marks.map((m) => ({ ...point(angleOf(m.at), MARK_R), text: m.text, key: m.at }))
	);

	function set(next: number) {
		if (disabled) return;
		const snapped = Math.round((next - min) / step) * step + min;
		const bounded = Math.min(max, Math.max(min, Number(snapped.toFixed(6))));
		if (bounded !== value) onchange(bounded);
	}

	function onkeydown(event: KeyboardEvent) {
		const small = keyStep ?? step;
		const big = Math.max(small, range / 10);
		const moves: Record<string, number> = {
			ArrowUp: clamped + small,
			ArrowRight: clamped + small,
			ArrowDown: clamped - small,
			ArrowLeft: clamped - small,
			PageUp: clamped + big,
			PageDown: clamped - big,
			Home: min,
			End: max
		};
		const next = moves[event.key];
		if (next === undefined) return;
		event.preventDefault();
		set(next);
	}

	// A notch is 100 px of wheel. A trackpad sends small deltas, so `wheelDelta`
	// keeps the pixels not yet made into a notch (positive while turning up).
	const NOTCH_PX = 100;
	let wheelDelta = 0;

	function onwheel(event: WheelEvent) {
		if (disabled || event.deltaY === 0) return;
		const up = event.deltaY < 0;
		// At the end of the range the page scrolls instead.
		if (up ? clamped >= max : clamped <= min) {
			wheelDelta = 0;
			return;
		}
		event.preventDefault();
		// deltaMode 1 counts lines (about 33 px each) and 2 counts pages.
		const unit = event.deltaMode === 2 ? window.innerHeight : event.deltaMode === 1 ? 33 : 1;
		const px = Math.abs(event.deltaY) * unit * (up ? 1 : -1);
		if (Math.sign(px) !== Math.sign(wheelDelta)) wheelDelta = 0;
		wheelDelta += px;
		const notches = Math.trunc(wheelDelta / NOTCH_PX);
		if (notches === 0) return;
		wheelDelta -= notches * NOTCH_PX;
		set(clamped + notches * (keyStep ?? step));
	}

	// One drag turns by height, or round the edge when it starts near the rim.
	let drag: {
		id: number;
		circular: boolean;
		startY: number;
		startValue: number;
		lastAngle: number;
		turned: number;
	} | null = null;
	let dialEl: HTMLDivElement | undefined = $state();

	function pointerAngle(event: PointerEvent): number {
		const box = dialEl?.getBoundingClientRect();
		if (!box) return 0;
		const dx = event.clientX - (box.left + box.width / 2);
		const dy = event.clientY - (box.top + box.height / 2);
		return (Math.atan2(dx, -dy) * 180) / Math.PI;
	}

	function onpointerdown(event: PointerEvent) {
		if (disabled || event.button !== 0 || !dialEl) return;
		event.preventDefault();
		dialEl.focus();
		dialEl.setPointerCapture(event.pointerId);
		const box = dialEl.getBoundingClientRect();
		const dx = event.clientX - (box.left + box.width / 2);
		const dy = event.clientY - (box.top + box.height / 2);
		drag = {
			id: event.pointerId,
			circular: Math.hypot(dx, dy) > (box.width / 2) * 0.6,
			startY: event.clientY,
			startValue: clamped,
			lastAngle: pointerAngle(event),
			turned: 0
		};
	}

	function onpointermove(event: PointerEvent) {
		if (!drag || event.pointerId !== drag.id) return;
		if (drag.circular) {
			const now = pointerAngle(event);
			let delta = now - drag.lastAngle;
			if (delta > 180) delta -= 360;
			if (delta < -180) delta += 360;
			drag.lastAngle = now;
			drag.turned += delta;
			set(drag.startValue + (drag.turned / SWEEP) * range);
		} else {
			const travel = size === 'lg' ? 180 : 140;
			set(drag.startValue + ((drag.startY - event.clientY) / travel) * range);
		}
	}

	function endDrag(event: PointerEvent) {
		if (drag && event.pointerId === drag.id) drag = null;
	}

	const text = $derived(format(clamped));
</script>

<div
	class="dn-knob {className}"
	data-size={size}
	class:is-disabled={disabled}
	title={disabled ? disabledReason : undefined}
>
	<div class="dn-knob__face" {onwheel}>
		<svg class="dn-knob__scale" viewBox="0 0 120 120" aria-hidden="true">
			{#each scale as dot (dot.key)}
				<circle cx={dot.x} cy={dot.y} r={size === 'lg' ? 2.1 : 2.8} class:is-lit={dot.lit} />
			{/each}
			{#each printed as mark (mark.key)}
				<text x={mark.x} y={mark.y} text-anchor="middle" dominant-baseline="central"
					>{mark.text}</text
				>
			{/each}
		</svg>
		<div
			bind:this={dialEl}
			class="dn-knob__dial"
			role="slider"
			tabindex={disabled ? -1 : 0}
			aria-label={label}
			aria-valuemin={min}
			aria-valuemax={max}
			aria-valuenow={clamped}
			aria-valuetext={text}
			aria-disabled={disabled || undefined}
			{onkeydown}
			{onpointerdown}
			{onpointermove}
			onpointerup={endDrag}
			onpointercancel={endDrag}
			onpointerleave={() => (wheelDelta = 0)}
			ondblclick={() => resetTo !== undefined && set(resetTo)}
		>
			<div class="dn-knob__skirt" style:rotate="{angle}deg"></div>
			<div class="dn-knob__cap"></div>
			<div class="dn-knob__pointer" style:rotate="{angle}deg">
				<span class="dn-knob__tick"></span>
			</div>
		</div>
	</div>
	<div class="dn-knob__caption" aria-hidden="true">
		<span class="dn-knob__label">{label}</span>
		<span class="dn-knob__value">{text}</span>
	</div>
</div>

<style>
	.dn-knob {
		--knob-lit: var(--color-primary);
		display: inline-flex;
		flex-direction: column;
		align-items: center;
		gap: 0.25rem;
	}
	.dn-knob[data-size='lg'] {
		--knob-size: 8.5rem;
	}
	.dn-knob[data-size='sm'] {
		--knob-size: 5rem;
	}

	.dn-knob__face {
		position: relative;
		width: var(--knob-size);
		aspect-ratio: 1;
	}

	.dn-knob__scale {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		overflow: visible;
	}
	.dn-knob__scale circle {
		fill: color-mix(in oklab, var(--color-base-content) 28%, transparent);
		transition: fill 120ms;
	}
	.dn-knob__scale circle.is-lit {
		fill: var(--knob-lit);
	}
	.dn-knob__scale text {
		fill: var(--color-muted);
		font-family: var(--font-mono);
		font-size: 8px;
		font-weight: 600;
	}
	.dn-knob[data-size='sm'] .dn-knob__scale text {
		font-size: 11px;
	}

	/* the dial: always black, like a record, so the acid tick reads on both decks */
	.dn-knob__dial {
		position: absolute;
		inset: 21%;
		border-radius: 9999px;
		cursor: grab;
		touch-action: none;
		box-shadow:
			0 0.5rem 1rem -0.35rem rgb(0 0 0 / 0.75),
			0 0 0 1px rgb(0 0 0 / 0.5);
	}
	.dn-knob__dial:active {
		cursor: grabbing;
	}
	.dn-knob__dial:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 4px;
	}

	/* the knurled edge and the tick turn; the light on the cap stays put */
	.dn-knob__skirt,
	.dn-knob__pointer {
		position: absolute;
		inset: 0;
		border-radius: inherit;
		transition: rotate 60ms linear;
	}
	.dn-knob__pointer {
		pointer-events: none;
	}
	.dn-knob__skirt {
		background:
			repeating-conic-gradient(rgb(255 255 255 / 0.13) 0 1.5deg, transparent 1.5deg 6deg),
			radial-gradient(circle, var(--dn-vinyl-groove) 0 60%, var(--dn-vinyl) 100%);
	}
	.dn-knob__tick {
		position: absolute;
		left: 50%;
		top: 5%;
		width: 9%;
		height: 30%;
		translate: -50% 0;
		border-radius: 9999px;
		background: var(--dn-action);
		box-shadow: 0 0 0.4rem color-mix(in oklab, var(--dn-action) 60%, transparent);
	}
	.dn-knob__cap {
		position: absolute;
		inset: 15%;
		border-radius: inherit;
		pointer-events: none;
		background:
			conic-gradient(
				from 200deg,
				rgb(255 255 255 / 0.16),
				transparent 22%,
				rgb(255 255 255 / 0.07) 50%,
				transparent 74%,
				rgb(255 255 255 / 0.16)
			),
			radial-gradient(circle at 40% 32%, rgb(255 255 255 / 0.14), transparent 62%), var(--dn-vinyl);
		box-shadow:
			inset 0 1px 0 rgb(255 255 255 / 0.12),
			inset 0 -2px 4px rgb(0 0 0 / 0.6);
	}
	.dn-knob__caption {
		display: flex;
		flex-direction: column;
		align-items: center;
		line-height: 1.15;
		font-family: var(--font-mono);
		text-transform: uppercase;
	}
	.dn-knob__label {
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		color: var(--color-base-content);
	}
	.dn-knob__value {
		font-size: 0.75rem;
		font-variant-numeric: tabular-nums;
		color: var(--color-muted);
	}

	.dn-knob.is-disabled .dn-knob__dial {
		cursor: not-allowed;
		filter: grayscale(1);
	}
	.dn-knob.is-disabled .dn-knob__tick {
		background: var(--color-muted);
		box-shadow: none;
	}

	@media (forced-colors: active) {
		.dn-knob__dial {
			border: 2px solid CanvasText;
		}
		.dn-knob__tick {
			background: Highlight;
		}
		.dn-knob__scale circle.is-lit {
			fill: Highlight;
		}
	}
</style>
