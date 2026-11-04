<!--
	A cover with a record half out of the sleeve behind it. The record is
	decoration only (hidden from screen readers, still, gone under high
	contrast); the cover, passed as children, is never filtered or covered.
	`class` sizes the whole piece: the cover takes 72% of its width and the
	record the rest. On a phone the record is smaller, and under 480px it
	goes so the cover keeps the room.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		/** Layout only: the width of cover and record together. */
		class?: string;
		children: Snippet;
	}

	let { class: className = '', children }: Props = $props();
</script>

<div class="dn-sleeve {className}">
	<div class="dn-decor dn-sleeve__record" aria-hidden="true"></div>
	<div class="dn-sleeve__cover">
		{@render children()}
	</div>
</div>

<style>
	.dn-sleeve {
		position: relative;
		container-type: inline-size;
	}

	.dn-sleeve__cover {
		position: relative;
		z-index: 1;
		width: 72cqw;
	}

	.dn-sleeve__record {
		position: absolute;
		top: 50%;
		left: 30cqw;
		width: 68cqw;
		aspect-ratio: 1;
		translate: 0 -50%;
		border-radius: 9999px;
		pointer-events: none;
		background:
			radial-gradient(circle, var(--dn-vinyl) 0 3%, transparent 3.5%),
			radial-gradient(circle, var(--dn-action) 0 34%, transparent 34.5%),
			repeating-radial-gradient(
				circle,
				var(--dn-vinyl) 0 0.35cqw,
				var(--dn-vinyl-groove) 0.35cqw 0.45cqw
			);
		box-shadow:
			0 0 0 1px var(--dn-vinyl-groove),
			0 12px 32px -12px rgb(0 0 0 / 0.55);
	}

	@media (max-width: 767px) {
		.dn-sleeve__cover {
			width: 80cqw;
		}
		.dn-sleeve__record {
			left: 32cqw;
			width: 64cqw;
		}
	}

	@media (max-width: 479px) {
		.dn-sleeve__cover {
			width: 100cqw;
		}
		.dn-sleeve__record {
			display: none;
		}
	}
</style>
