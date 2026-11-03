<script lang="ts">
	import { serviceStatusStore } from '$lib/stores/serviceStatus';
	import { fromStore } from 'svelte/store';
	import { TriangleAlert, X } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	const status = fromStore(serviceStatusStore);

	let dismissed = $state(false);

	const degradedSources = $derived(Object.keys(status.current));
	const hasDegradation = $derived(degradedSources.length > 0 && !dismissed);

	const sourceLabel = $derived(
		degradedSources.map((s) => s.charAt(0).toUpperCase() + s.slice(1)).join(', ')
	);

	const verb = $derived(degradedSources.length > 1 ? 'are' : 'is');

	$effect(() => {
		if (degradedSources.length > 0) dismissed = false;
	});
</script>

{#if hasDegradation}
	<div
		class="fixed top-0 left-0 right-0 z-[115] flex items-center justify-center pointer-events-none"
	>
		<div
			class="alert alert-warning shadow-lg mx-auto mt-2 max-w-xl pointer-events-auto text-sm gap-2 py-2"
			role="status"
		>
			<TriangleAlert class="h-4 w-4 shrink-0" />
			<span>{sourceLabel} {verb} unavailable, so some results may be missing</span>
			<IconButton size="xs" icon={X} label="Dismiss" onclick={() => (dismissed = true)} />
		</div>
	</div>
{/if}
