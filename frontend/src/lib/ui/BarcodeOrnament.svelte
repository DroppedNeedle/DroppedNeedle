<!--
	A decorative barcode in the current text colour, at most once on a page.
	It encodes nothing and never stands for a real release number, so it is
	hidden from screen readers; it goes under high contrast and on a phone.
	`class` sets the height and the colour.
-->
<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		class?: string;
	}

	let { class: className = '' }: Props = $props();

	const url = `url("${withBasePath('/img/barcode.svg')}")`;
</script>

<span
	class="dn-decor dn-barcode {className}"
	aria-hidden="true"
	style:mask-image={url}
	style:-webkit-mask-image={url}
></span>

<style>
	.dn-barcode {
		display: block;
		aspect-ratio: 480 / 110;
		pointer-events: none;
		background-color: currentColor;
		mask-size: contain;
		mask-repeat: no-repeat;
		-webkit-mask-size: contain;
		-webkit-mask-repeat: no-repeat;
	}

	@media (max-width: 479px) {
		.dn-barcode {
			display: none;
		}
	}
</style>
