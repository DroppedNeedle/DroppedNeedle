<script lang="ts">
	import type { RemoteSourceDescriptor } from '$lib/features/remote-library/queries/remoteSources';

	let { remote, class: className = '' }: { remote: RemoteSourceDescriptor; class?: string } =
		$props();
</script>

<div
	class="h-[2px] rounded-full bg-gradient-to-r from-transparent to-transparent opacity-40 {remote.line} {className}"
></div>
