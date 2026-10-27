<script lang="ts">
	import type { PluginSourceHealth } from '$lib/queries/plugins/PluginSourceQueries.svelte';

	interface Props {
		health: PluginSourceHealth;
	}

	let { health }: Props = $props();

	const looks: Record<PluginSourceHealth, { label: string; tone: string }> = {
		ok: { label: 'Healthy', tone: 'badge-success' },
		degraded: { label: 'Restarting', tone: 'badge-warning' },
		error: { label: 'Error', tone: 'badge-error' },
		unknown: { label: 'Checking', tone: 'badge-ghost' }
	};
	const look = $derived(looks[health]);
</script>

<span class="badge badge-sm badge-soft {look.tone}">{look.label}</span>
