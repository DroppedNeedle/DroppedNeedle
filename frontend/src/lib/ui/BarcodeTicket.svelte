<!--
	A tilted paper ticket: a barcode, a mono label under it ("DN / CC 3.2")
	and a block of diagonal stripes. It is the design's signature, not data:
	the bars encode nothing, so it is hidden from screen readers and gone
	under high contrast. `class` sets its width; everything inside scales
	with it.
-->
<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		label: string;
		/** Layout only: width, margin. */
		class?: string;
	}

	let { label, class: className = '' }: Props = $props();

	const bars = `url("${withBasePath('/img/barcode.svg')}")`;
	// a longer label prints smaller, so it always fits beside the stripes
	const labelSize = $derived(`${Math.min(8, 90 / Math.max(label.length, 1)).toFixed(2)}cqw`);
</script>

<div class="dn-decor dn-ticket {className}" aria-hidden="true">
	<div class="dn-ticket__stock">
		<span class="dn-ticket__bars" style:mask-image={bars} style:-webkit-mask-image={bars}></span>
		<span class="dn-ticket__foot">
			<span class="dn-ticket__label" style:font-size={labelSize}>{label}</span>
			<span class="dn-ticket__stripes dn-stripes"></span>
		</span>
	</div>
</div>

<style>
	.dn-ticket {
		container-type: inline-size;
		pointer-events: none;
	}

	.dn-ticket__stock {
		display: flex;
		flex-direction: column;
		gap: 2.5cqw;
		padding: 4cqw 5cqw 4.5cqw;
		border-radius: 2px;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		box-shadow:
			0 0 0 1px color-mix(in oklab, var(--dn-paper-content) 12%, transparent),
			4px 10px 22px -10px rgb(0 0 0 / 0.7);
		rotate: -3deg;
	}

	.dn-ticket__bars {
		display: block;
		height: 18cqw;
		background-color: currentColor;
		mask-size: 100% 100%;
		mask-repeat: no-repeat;
		-webkit-mask-size: 100% 100%;
		-webkit-mask-repeat: no-repeat;
	}

	.dn-ticket__foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 3cqw;
	}

	.dn-ticket__label {
		font-family: var(--font-mono);
		min-width: 0;
		overflow: hidden;
		font-weight: 600;
		line-height: 1;
		letter-spacing: -0.01em;
		white-space: nowrap;
	}

	.dn-ticket__stripes {
		flex: 0 0 auto;
		width: 17cqw;
		height: 7.5cqw;
		font-size: 8cqw;
	}
</style>
