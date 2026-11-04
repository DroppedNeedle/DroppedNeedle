<!--
	The DroppedNeedle logo in the current text colour: the logo image is a
	mask over `currentColor`, so it is off-white in dark and forest in light
	with no filter. `wide` is the mark and the name, `icon` the mark alone.
	`label` names it; pass an empty one where text beside it already does.
	`class` sets the height and the colour.
-->
<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		variant?: 'wide' | 'icon';
		label?: string;
		class?: string;
	}

	let { variant = 'wide', label = 'DroppedNeedle', class: className = '' }: Props = $props();

	const files = { wide: '/logo_wide_white.png', icon: '/logo_icon.png' };
	// Both images are 334 pixels high.
	const ratios = { wide: '1274 / 334', icon: '641 / 334' };
	const url = $derived(`url("${withBasePath(files[variant])}")`);
</script>

<span
	class="dn-logo-mask block shrink-0 {className}"
	role={label ? 'img' : undefined}
	aria-label={label || undefined}
	aria-hidden={label ? undefined : 'true'}
	style:aspect-ratio={ratios[variant]}
	style:mask-image={url}
	style:-webkit-mask-image={url}
></span>

<style>
	.dn-logo-mask {
		background-color: currentColor;
		mask-repeat: no-repeat;
		mask-size: contain;
		mask-position: left center;
		-webkit-mask-repeat: no-repeat;
		-webkit-mask-size: contain;
		-webkit-mask-position: left center;
	}

	/* forced colours drop background colours; show the mark in the text colour */
	@media (forced-colors: active) {
		.dn-logo-mask {
			background-color: CanvasText;
			forced-color-adjust: none;
		}
	}
</style>
