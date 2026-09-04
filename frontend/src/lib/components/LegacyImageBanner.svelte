<script lang="ts">
	import { getVersionQuery } from '$lib/queries/VersionQuery.svelte';
	import { TriangleAlert, X } from 'lucide-svelte';

	const versionQuery = getVersionQuery();

	const showForVersion = $derived(
		(versionQuery.data?.version ?? '').replace(/^v/i, '').startsWith('2.11.')
	);

	let dismissed = $state(false);
</script>

{#if showForVersion && !dismissed}
	<div class="alert alert-warning w-full rounded-none px-4 py-3" role="alert">
		<TriangleAlert class="h-6 w-6 shrink-0" aria-hidden="true" />
		<div class="flex-1">
			<p class="font-bold">Old image names are retired</p>
			<p class="text-sm">
				If you're still pulling an old <code>habirabbu/*</code> image, switch your compose file to
				<code>droppedneedle/droppedneedle</code>
				or
				<code>ghcr.io/droppedneedle/droppedneedle</code>, then pull and restart. Old names stop
				receiving updates after v2.11.x. Already switched? You're all set - your library and
				settings are untouched.
			</p>
		</div>
		<button
			class="btn btn-ghost btn-xs btn-circle"
			onclick={() => (dismissed = true)}
			aria-label="Dismiss"
		>
			<X class="h-3 w-3" />
		</button>
	</div>
{/if}
