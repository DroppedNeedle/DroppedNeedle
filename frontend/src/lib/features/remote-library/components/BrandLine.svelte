<script lang="ts">
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';

	let { remote, class: className = '' }: { remote: RemoteSourceDescriptor; class?: string } =
		$props();
</script>

<!-- the server's brand colour, as a hairline under the page top -->
<div
	class="hub-brand-line {className}"
	style:--brand="rgb(var(--brand-{remote.source}))"
	aria-hidden="true"
></div>

<style>
	.hub-brand-line {
		height: 3px;
		border-radius: 999px;
		background: linear-gradient(
			to right,
			transparent,
			var(--brand) 10%,
			var(--brand) 90%,
			transparent
		);
	}
</style>
