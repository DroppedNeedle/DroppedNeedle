<script lang="ts">
	import { type MusicSource } from '$lib/stores/musicSource';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		currentSource: MusicSource;
		onSourceChange?: (source: MusicSource) => void;
	}

	let { currentSource, onSourceChange }: Props = $props();

	const connectionsQuery = getConnectionsQuery();
	const connections = $derived(connectionsQuery.data?.connections ?? []);

	let switching = $state(false);

	let lbEnabled = $derived(connections.some((c) => c.service === 'listenbrainz'));
	let lfmEnabled = $derived(connections.some((c) => c.service === 'lastfm'));
	let showSwitcher = $derived(lbEnabled && lfmEnabled);

	async function handleSwitch(source: MusicSource) {
		if (source === currentSource || switching) return;
		switching = true;
		onSourceChange?.(source);
		switching = false;
	}
</script>

{#if showSwitcher}
	<div class="join">
		<Button
			variant={currentSource === 'listenbrainz' ? 'primary' : 'ghost'}
			size="sm"
			class="join-item"
			aria-pressed={currentSource === 'listenbrainz'}
			disabled={switching}
			onclick={() => handleSwitch('listenbrainz')}
		>
			ListenBrainz
		</Button>
		<Button
			variant={currentSource === 'lastfm' ? 'lastfm' : 'ghost'}
			size="sm"
			class="join-item"
			aria-pressed={currentSource === 'lastfm'}
			disabled={switching}
			onclick={() => handleSwitch('lastfm')}
		>
			Last.fm
		</Button>
	</div>
{/if}
